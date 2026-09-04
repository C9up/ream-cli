//! Generator — code generation from templates (pure Rust, no Node.js).
//!
//! Every generator plans first, then either writes the plan to disk or emits it
//! as JSON for the MCP `dryRun` path. A run that would clobber a file refuses as
//! a whole before touching anything, and a failure part-way through restores
//! what it had already written.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path};

use serde_json::json;

const MAX_NAME_LEN: usize = 128;

#[derive(Debug, Clone)]
pub struct PlannedFile {
    pub path: String,
    pub content: String,
    pub exists: bool,
}

#[derive(Debug, Default)]
pub struct Plan {
    pub files: Vec<PlannedFile>,
    pub warnings: Vec<String>,
}

/// PascalCase rule for class-style names — `^[A-Z][A-Za-z0-9]*$`.
pub fn validate_class_name(s: &str, label: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err(format!("{label} must not be empty"));
    }
    if s.len() > MAX_NAME_LEN {
        return Err(format!(
            "{label} exceeds maximum length of {MAX_NAME_LEN} characters"
        ));
    }
    let mut chars = s.chars();
    let first = chars.next().expect("non-empty checked above");
    if !first.is_ascii_uppercase() {
        return Err(format!(
            "{label} '{s}' must start with an ASCII uppercase letter (PascalCase)"
        ));
    }
    if !chars.all(|c| c.is_ascii_alphanumeric()) {
        return Err(format!(
            "{label} '{s}' must contain only ASCII letters and digits (PascalCase)"
        ));
    }
    Ok(())
}

/// Kebab-case rule for module-path identifiers — `^[a-z][a-z0-9-]*$`.
pub fn validate_module_name(s: &str, label: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err(format!("{label} must not be empty"));
    }
    if s.len() > MAX_NAME_LEN {
        return Err(format!(
            "{label} exceeds maximum length of {MAX_NAME_LEN} characters"
        ));
    }
    let mut chars = s.chars();
    let first = chars.next().expect("non-empty checked above");
    if !first.is_ascii_lowercase() {
        return Err(format!(
            "{label} '{s}' must start with a lowercase ASCII letter (kebab-case)"
        ));
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err(format!(
            "{label} '{s}' must contain only lowercase ASCII letters, digits or '-' (kebab-case)"
        ));
    }
    Ok(())
}

fn validate_path(path: &str) -> Result<(), String> {
    for component in Path::new(path).components() {
        if matches!(component, Component::ParentDir) {
            return Err(format!("Refusing to write outside project root: {path}"));
        }
    }
    Ok(())
}

/// Plan a write OR perform it on disk depending on `dry`.
///
/// In dry mode, populates `plan.files` with `{path, content, exists}` and
/// touches nothing. In write mode, refuses to overwrite an existing file
/// unless `force` is true; emits `created`/`modified` paths via the return
/// channel.
#[derive(Debug)]
pub enum WriteOutcome {
    Created(String),
    Modified(String),
    Conflict(String),
}

pub fn plan_or_write(
    plan: &mut Plan,
    dry: bool,
    force: bool,
    path: &str,
    content: &str,
) -> Result<WriteOutcome, String> {
    validate_path(path)?;
    let full_path = Path::new(path);
    let exists = full_path.exists();

    if dry {
        plan.files.push(PlannedFile {
            path: path.to_string(),
            content: content.to_string(),
            exists,
        });
        return Ok(if exists {
            WriteOutcome::Modified(path.to_string())
        } else {
            WriteOutcome::Created(path.to_string())
        });
    }

    if exists && !force {
        return Ok(WriteOutcome::Conflict(path.to_string()));
    }

    // Defense-in-depth: refuse to follow a symlinked LEAF — even with
    // `--force`. A pre-existing symlink at `full_path` pointing outside
    // the project would otherwise let `fs::write` clobber whatever the
    // link targets.
    if let Ok(meta) = fs::symlink_metadata(full_path) {
        if meta.file_type().is_symlink() {
            return Err(format!("refusing to write through a symlink: {path}"));
        }
    }

    if let Some(parent) = full_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create directory: {e}"))?;
        if let Ok(cwd) = std::env::current_dir() {
            if let (Ok(canon_root), Ok(canon_parent)) =
                (fs::canonicalize(&cwd), fs::canonicalize(parent))
            {
                if !canon_parent.starts_with(&canon_root) {
                    return Err(format!(
                        "refusing to write outside project root (symlink): {path}"
                    ));
                }
            }
        }
    }

    fs::write(full_path, content).map_err(|e| format!("Failed to write file: {e}"))?;
    eprintln!(
        "  \x1b[32m{}\x1b[0m {path}",
        if exists { "modified" } else { "created" }
    );
    Ok(if exists {
        WriteOutcome::Modified(path.to_string())
    } else {
        WriteOutcome::Created(path.to_string())
    })
}

/// Top-level generator dispatcher — invoked by `main.rs`.
///
/// Validates inputs, builds the plan via per-kind helpers, then either
/// returns the plan (dry-run) or commits it to disk. Always emits a single
/// JSON line to stdout describing the outcome (for the MCP wrapper).
pub fn make(
    kind: &str,
    module: &str,
    name: &str,
    dry_run: bool,
    force: bool,
) -> Result<(), String> {
    if !module.is_empty() {
        validate_module_name(module, "module")?;
    }

    match kind {
        "controller" | "entity" | "validator" | "seeder" => {
            validate_class_name(name, "name")?;
        }
        "service" | "provider" | "migration" => {
            validate_name_relaxed(name, "name")?;
        }
        "command" => {
            validate_command_name(name)?;
        }
        _ => {}
    }

    let entries = build_plan_entries(kind, module, name)?;
    flush_outcome(entries, dry_run, force)
}

/// Generators that take one extra option — `make:middleware --stack`,
/// `make:listener --event`.
///
/// Kept apart from {@link make} rather than threading an `Option` through every
/// generator: only these three take one, and widening the shared signature
/// would touch a dozen call sites for nothing.
pub fn make_with_option(
    kind: &str,
    name: &str,
    option: Option<&str>,
    dry_run: bool,
    force: bool,
) -> Result<(), String> {
    validate_name_relaxed(name, "name")?;

    let entry = match kind {
        "middleware" => {
            let stack = option.unwrap_or("router");
            if !matches!(stack, "server" | "named" | "router") {
                return Err(format!(
                    "unknown --stack '{stack}' (expected: server, named, or router)"
                ));
            }
            generate_middleware(name, stack)?
        }
        "event" => generate_event(name)?,
        "listener" => {
            if let Some(event) = option {
                validate_name_relaxed(event, "--event")?;
            }
            generate_listener(name, option)
        }
        other => return Err(format!("Unknown generator type: {other}")),
    };

    flush_outcome(vec![entry], dry_run, force)
}

/// `make:module` umbrella — emits entity + controller + migration + validator.
///
/// Emits the scope-cut warnings the spec promises:
///   - Existing `app/<module>/index.ts` barrel exports are NOT updated.
///   - Migration filenames are `YYYYMMDDNNN_name.ts` (date + per-day sequence;
///     see `next_migration_sequence`), so they sort chronologically.
pub fn make_module(module: &str, name: &str, dry_run: bool, force: bool) -> Result<(), String> {
    validate_module_name(module, "module")?;
    validate_class_name(name, "name")?;

    let entries: Vec<(String, String)> = vec![
        generate_entity(module, name)?,
        generate_controller(module, name)?,
        generate_validator(module, name)?,
        generate_migration(name)?,
    ];

    let mut warnings: Vec<String> = Vec::new();
    let barrel = format!("app/{module}/index.ts");
    if Path::new(&barrel).exists() {
        warnings.push(format!(
            "barrel export at `{barrel}` was NOT updated automatically — append the new symbols manually."
        ));
    }
    warnings.push(
        "migration filename uses today's date + a per-day sequence (YYYYMMDDNNN); verify ordering matches your intent.".to_string(),
    );

    flush_outcome_with_warnings(entries, warnings, dry_run, force)
}

/// Relaxed name rule for `provider`/`migration` where the user
/// historically passed snake_case or PascalCase. Still rejects path-traversal,
/// shell metacharacters, AND leading `-`/`_` (clap would parse `-evil` as a
/// flag, and `_hidden` produces hidden filenames).
fn validate_name_relaxed(s: &str, label: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err(format!("{label} must not be empty"));
    }
    if s.len() > MAX_NAME_LEN {
        return Err(format!(
            "{label} exceeds maximum length of {MAX_NAME_LEN} characters"
        ));
    }
    let first = s.chars().next().expect("non-empty checked above");
    if first == '-' || first == '_' {
        return Err(format!("{label} '{s}' must not start with '-' or '_'"));
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "{label} '{s}' contains invalid characters (alphanumeric, '-' or '_' only)"
        ));
    }
    Ok(())
}

/// Command names carry a namespace separator — `make:controller`,
/// `app:provision`. Same rules as {@link validate_name_relaxed} otherwise, plus
/// `:` in an inner position: a leading or trailing colon would produce an empty
/// namespace or an empty name, and a doubled one an empty segment.
fn validate_command_name(s: &str) -> Result<(), String> {
    if s.starts_with(':') || s.ends_with(':') || s.contains("::") {
        return Err(format!(
            "name '{s}' has an empty namespace segment — use `namespace:name`"
        ));
    }
    validate_name_relaxed(&s.replace(':', "-"), "name")
}

fn build_plan_entries(
    kind: &str,
    module: &str,
    name: &str,
) -> Result<Vec<(String, String)>, String> {
    let entry = match kind {
        "service" => {
            require_module(module, kind)?;
            generate_service(module, name)?
        }
        "entity" => {
            require_module(module, kind)?;
            generate_entity(module, name)?
        }
        "controller" => {
            require_module(module, kind)?;
            generate_controller(module, name)?
        }
        "validator" => {
            require_module(module, kind)?;
            generate_validator(module, name)?
        }
        "provider" => generate_provider(name)?,
        "command" => generate_command(name)?,
        "migration" => generate_migration(name)?,
        "seeder" => {
            // Module is OPTIONAL for seeders — they live under
            // `database/seeders/`, not `app/<module>/`. When provided,
            // the module name is stamped into the seeder JSDoc for
            // traceability; when omitted we just emit the file.
            generate_seeder(module, name)?
        }
        other => return Err(format!("Unknown generator type: {other}")),
    };
    Ok(vec![entry])
}

fn require_module(module: &str, kind: &str) -> Result<(), String> {
    if module.is_empty() {
        return Err(format!(
            "module argument is required for `make:{kind}` (e.g. `ream make:{kind} <module> <Name>`)"
        ));
    }
    Ok(())
}

fn flush_outcome(entries: Vec<(String, String)>, dry_run: bool, force: bool) -> Result<(), String> {
    flush_outcome_with_warnings(entries, Vec::new(), dry_run, force)
}

/// Write planned files transactionally — staging-then-commit so that
/// a failure on file N doesn't leave files 1..N-1 dangling on disk.
///
/// Phase 1 (PLAN): walk every entry through `plan_or_write` in dry mode
/// to detect conflicts and collect the list. Any conflict short-circuits
/// to the conflict-JSON path with NO disk writes.
///
/// Phase 2 (WRITE): only entered if Phase 1 was clean; commits each
/// file in order. If a write fails partway through, every file already
/// committed in this call is restored to its prior state (deleted if it
/// was newly created; restored from snapshot if it was overwritten).
fn flush_outcome_with_warnings(
    entries: Vec<(String, String)>,
    warnings: Vec<String>,
    dry_run: bool,
    force: bool,
) -> Result<(), String> {
    let mut plan = Plan {
        files: Vec::new(),
        warnings,
    };

    if dry_run {
        // Dry-run path: populate plan only, never touch disk.
        for (path, content) in &entries {
            plan_or_write(&mut plan, true, force, path, content)?;
        }
        let files: Vec<_> = plan
            .files
            .iter()
            .map(|f| {
                json!({
                    "path": f.path,
                    "content": f.content,
                    "exists": f.exists,
                })
            })
            .collect();
        let payload = json!({
            "files": files,
            "warnings": plan.warnings,
        });
        println!("{payload}");
        return Ok(());
    }

    // PHASE 1 — pre-flight: detect conflicts before touching the disk.
    let conflicts: Vec<String> = entries
        .iter()
        .filter_map(|(p, _)| {
            if Path::new(p).exists() && !force {
                Some(p.clone())
            } else {
                None
            }
        })
        .collect();
    if !conflicts.is_empty() {
        let payload = json!({
            "error": "files already exist",
            "hint": "set --force to overwrite",
            "conflicts": conflicts,
        });
        println!("{payload}");
        return Err(format!(
            "files already exist: {} (use --force to overwrite)",
            conflicts.join(", ")
        ));
    }

    // PHASE 2 — commit. Snapshot any pre-existing file before
    // overwriting, so we can roll back if a later write fails.
    let mut rollbacks: Vec<(String, Option<Vec<u8>>)> = Vec::new();
    let mut created: Vec<String> = Vec::new();
    let mut modified: Vec<String> = Vec::new();

    for (path, content) in &entries {
        let prior = if Path::new(path).exists() {
            fs::read(path).ok()
        } else {
            None
        };

        match plan_or_write(&mut plan, false, force, path, content) {
            Ok(WriteOutcome::Created(p)) => {
                rollbacks.push((p.clone(), None));
                created.push(p);
            }
            Ok(WriteOutcome::Modified(p)) => {
                rollbacks.push((p.clone(), prior));
                modified.push(p);
            }
            Ok(WriteOutcome::Conflict(p)) => {
                rollback_writes(&rollbacks);
                return Err(format!(
                    "internal: unexpected conflict on `{p}` after phase-1 pre-flight"
                ));
            }
            Err(err) => {
                rollback_writes(&rollbacks);
                return Err(format!("write failed on `{path}`: {err}"));
            }
        }
    }

    let payload = json!({
        "createdFiles": created,
        "modifiedFiles": modified,
        "warnings": plan.warnings,
    });
    println!("{payload}");
    Ok(())
}

fn rollback_writes(rollbacks: &[(String, Option<Vec<u8>>)]) {
    for (path, prior) in rollbacks.iter().rev() {
        match prior {
            Some(bytes) => {
                if let Err(e) = fs::write(path, bytes) {
                    eprintln!("  \x1b[33mrollback\x1b[0m: failed to restore {path}: {e}");
                } else {
                    eprintln!("  \x1b[33mrollback\x1b[0m: restored {path}");
                }
            }
            None => {
                if let Err(e) = fs::remove_file(path) {
                    eprintln!("  \x1b[33mrollback\x1b[0m: failed to remove {path}: {e}");
                } else {
                    eprintln!("  \x1b[33mrollback\x1b[0m: removed {path}");
                }
            }
        }
    }
}

fn ensure_suffix(name: &str, suffix: &str) -> String {
    if name.ends_with(suffix) {
        name.to_string()
    } else {
        format!("{name}{suffix}")
    }
}

fn to_pascal_case(name: &str) -> String {
    name.split(['_', '-', ' '])
        .filter(|s| !s.is_empty())
        .map(|s| {
            let mut chars = s.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().chain(chars).collect(),
            }
        })
        .collect()
}

fn to_snake_case(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut result = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() {
            let prev_lower = i > 0 && chars[i - 1].is_lowercase();
            let next_lower = i + 1 < chars.len() && chars[i + 1].is_lowercase();
            let preceded_by_upper = i > 0 && chars[i - 1].is_uppercase();
            if i > 0 && (prev_lower || (next_lower && preceded_by_upper)) {
                result.push('_');
            }
            for lc in c.to_lowercase() {
                result.push(lc);
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// One generated file, before a published stub gets a say.
///
/// The body carries `{{ placeholders }}` rather than values already
/// interpolated, so the copy `stubs:publish` writes IS this text — not a
/// reconstruction of it.
///
/// It used to be a reconstruction: the generator ran with sentinel names and
/// each sentinel was swapped back out for the placeholder it stood for. Two
/// variables whose sentinel rendered to the same string were then
/// indistinguishable, and `make:command` published
/// `commandName = '{{ className }}'` — so an app that published that stub
/// generated `commandName = 'AppProvision'` for `ream make:command
/// app:provision`, a command unreachable under the name it was asked for and
/// invisible to the scan that lets an app override a built-in.
struct Template {
    /// Where it is written when the stub does not redirect itself.
    path: String,
    /// The text, with `{{ placeholder }}` names.
    body: &'static str,
    /// What each placeholder stands for.
    vars: BTreeMap<&'static str, String>,
}

impl Template {
    /// The built-in output, or the app's published stub when it has one.
    ///
    /// A published stub may redirect its own output, so the path comes back
    /// from the resolver rather than being decided here. A broken stub is an
    /// error rather than a silent fall back to the built-in: the generated file
    /// would look right and ignore the edit.
    fn resolve(self, kind: &str) -> Result<(String, String), String> {
        let built_in = crate::stubs::render(self.body, &self.vars);
        let resolved = crate::stubs::resolve(kind, &self.vars, self.path, built_in)?;
        Ok((resolved.path, resolved.content))
    }
}

/// Build the variable map from `(name, value)` pairs.
fn vars_of<const N: usize>(pairs: [(&'static str, String); N]) -> BTreeMap<&'static str, String> {
    BTreeMap::from(pairs)
}

const SERVICE: &str = r#"import { Service } from '@c9up/ream'

@Service()
export class {{ className }} {
  async findAll() {
    return []
  }

  async findById(id: string) {
    return null
  }

  async create(data: Record<string, unknown>) {
    return data
  }

  async update(id: string, data: Record<string, unknown>) {
    return { id, ...data }
  }

  async delete(id: string) {
    return { id }
  }
}
"#;

fn service_template(module: &str, name: &str) -> Template {
    let class_name = ensure_suffix(name, "Service");
    Template {
        path: format!("app/{module}/{class_name}.ts"),
        body: SERVICE,
        vars: vars_of([
            ("className", class_name),
            ("module", module.to_string()),
            ("name", name.to_string()),
        ]),
    }
}

fn generate_service(module: &str, name: &str) -> Result<(String, String), String> {
    service_template(module, name).resolve("service")
}

const ENTITY: &str = r#"import { Entity, Column, PrimaryKey, BaseEntity } from '@c9up/atlas'

@Entity('{{ tableName }}')
export class {{ className }} extends BaseEntity {
  @PrimaryKey() id!: string
  @Column() createdAt!: string
  @Column() updatedAt!: string
}
"#;

fn entity_template(module: &str, name: &str) -> Template {
    let table_name = format!("{}s", to_snake_case(name));
    Template {
        path: format!("app/{module}/{name}.ts"),
        body: ENTITY,
        vars: vars_of([
            ("className", name.to_string()),
            ("module", module.to_string()),
            ("name", name.to_string()),
            ("tableName", table_name),
        ]),
    }
}

fn generate_entity(module: &str, name: &str) -> Result<(String, String), String> {
    entity_template(module, name).resolve("entity")
}

const CONTROLLER: &str = r#"import type { HttpContext } from '@c9up/ream'

export class {{ className }} {
  async index({ response }: HttpContext) {
    response.status(200).json([])
  }

  async show({ params, response }: HttpContext) {
    const { id } = params
    response.status(200).json({ id })
  }

  async store({ request, response }: HttpContext) {
    const data = request.body()
    response.status(201).json({ created: true })
  }

  async update({ params, response }: HttpContext) {
    const { id } = params
    response.status(200).json({ id, updated: true })
  }

  async destroy({ params, response }: HttpContext) {
    const { id } = params
    response.status(204).send('')
  }
}
"#;

fn controller_template(module: &str, name: &str) -> Template {
    let class_name = ensure_suffix(name, "Controller");
    Template {
        path: format!("app/{module}/{class_name}.ts"),
        body: CONTROLLER,
        vars: vars_of([
            ("className", class_name),
            ("module", module.to_string()),
            ("name", name.to_string()),
        ]),
    }
}

fn generate_controller(module: &str, name: &str) -> Result<(String, String), String> {
    controller_template(module, name).resolve("controller")
}

const VALIDATOR: &str = r#"import { rules, schema } from '@c9up/rune'

export const {{ className }} = schema({
  // Define validation rules
  // name: rules.string().min(1).max(255),
  // email: rules.string().email(),
})
"#;

fn validator_template(module: &str, name: &str) -> Template {
    let class_name = ensure_suffix(name, "Validator");
    Template {
        path: format!("app/{module}/{class_name}.ts"),
        body: VALIDATOR,
        vars: vars_of([
            ("className", class_name),
            ("module", module.to_string()),
            ("name", name.to_string()),
        ]),
    }
}

fn generate_validator(module: &str, name: &str) -> Result<(String, String), String> {
    validator_template(module, name).resolve("validator")
}

const PROVIDER: &str = r#"import { Provider } from '@c9up/ream'

export default class {{ className }} extends Provider {
  register() {
    // Register bindings in the container
  }

  async boot() {
    // Connect and verify
  }

  async start() {
    // Runs before HTTP server starts
  }

  async ready() {
    // Application operational
  }

  async shutdown() {
    // Cleanup
  }
}
"#;

fn provider_template(name: &str) -> Template {
    let class_name = ensure_suffix(name, "Provider");
    Template {
        path: format!("providers/{class_name}.ts"),
        body: PROVIDER,
        vars: vars_of([("className", class_name), ("name", name.to_string())]),
    }
}

fn generate_provider(name: &str) -> Result<(String, String), String> {
    provider_template(name).resolve("provider")
}

const COMMAND: &str = r#"import { BaseCommand, flags } from '@c9up/ream/console'
import type { CommandOptions } from '@c9up/ream/console'

export default class {{ className }} extends BaseCommand {
  static commandName = '{{ name }}'
  static description = 'TODO describe what this command does'

  /**
   * `startApp` boots providers and the container before `run()`. Off by
   * default: a command that only touches the filesystem has no
   * reason to open a database connection. Turn it on to reach `this.app`.
   */
  static options: CommandOptions = { startApp: false }

  @flags.boolean({ description: 'Report what would happen without doing it' })
  declare dryRun: boolean

  async run(): Promise<void> {
    if (this.dryRun) {
      this.logger.info('Dry run — nothing was changed.')
      return
    }

    this.logger.success('{{ name }} ran.')
  }
}
"#;

/// `make:command` — a console command in the app's `commands/` directory.
///
/// That directory is auto-discovered by the console kernel, so the generated
/// file is runnable as `ream <name>` with no registration step. `reamrc.ts`
/// `commands[]` stays reserved for commands shipped by packages, which
/// discovery cannot see.
///
/// `app:provision` is a valid COMMAND name but neither a valid class name nor a
/// valid file name, so both are derived (`app:provision` -> `AppProvision`,
/// `app-provision.ts`) while `commandName` keeps the name that was asked for.
fn command_template(name: &str) -> Template {
    let file_name = name.replace(':', "-");
    let class_name = to_pascal_case(&file_name);
    Template {
        path: format!("commands/{file_name}.ts"),
        body: COMMAND,
        vars: vars_of([
            ("className", class_name),
            ("fileName", file_name),
            ("name", name.to_string()),
        ]),
    }
}

fn generate_command(name: &str) -> Result<(String, String), String> {
    command_template(name).resolve("command")
}

const MIDDLEWARE: &str = r#"import type { HttpContext } from '@c9up/ream'

/**
 * Register it in `start/kernel.ts`:
 *   {{ registration }}
 */
export default class {{ className }} {
  async handle(ctx: HttpContext, next: () => Promise<void>) {
    // Runs before the route handler.
    await next()
    // Runs after it — the response is available here.
  }
}
"#;

/// `make:middleware` — an HTTP middleware class in `app/middleware/`.
///
/// The registration line is a variable rather than three literals chosen here,
/// so a published stub still answers to `--stack`. Written as literals it did
/// not: publishing froze whichever one the stub happened to be rendered with,
/// and `--stack server` then produced a `router.use` hint.
fn middleware_template(name: &str, stack: &str) -> Template {
    let base = to_snake_case(&strip_suffix_ci(name, "middleware"));
    // The `Middleware` suffix is this generator's one suffixed name; the file
    // is snake_case, matching what the scaffold already writes
    // (`app/middleware/auth_middleware.ts`).
    let class_name = format!("{}Middleware", to_pascal_case(&base));
    let import = format!("() => import('#middleware/{base}_middleware.js')");
    let registration = match stack {
        "server" => format!("server.use([{import}])"),
        "named" => format!("router.named({{ {base}: {import} }})"),
        _ => format!("router.use([{import}])"),
    };
    Template {
        path: format!("app/middleware/{base}_middleware.ts"),
        body: MIDDLEWARE,
        vars: vars_of([
            ("className", class_name),
            ("fileName", base),
            ("name", name.to_string()),
            ("registration", registration),
            ("stack", stack.to_string()),
        ]),
    }
}

fn generate_middleware(name: &str, stack: &str) -> Result<(String, String), String> {
    middleware_template(name, stack).resolve("middleware")
}

const EVENT: &str = r#"import { BaseEvent } from '@c9up/ream/events'

export default class {{ className }} extends BaseEvent {
  /** Name listeners subscribe to. Defaults to the class name when omitted. */
  static eventName = '{{ fileName }}'

  constructor(public payload: Record<string, unknown>) {
    super()
  }
}
"#;

/// `make:event` — a typed event class in `app/events/`.
///
/// No suffix here: `make:event orderShipped` generates `OrderShipped`, unlike
/// middleware, whose name carries one.
fn event_template(name: &str) -> Template {
    let base = to_snake_case(name);
    let class_name = to_pascal_case(&base);
    Template {
        path: format!("app/events/{base}.ts"),
        body: EVENT,
        vars: vars_of([
            ("className", class_name),
            ("fileName", base),
            ("name", name.to_string()),
        ]),
    }
}

fn generate_event(name: &str) -> Result<(String, String), String> {
    event_template(name).resolve("event")
}

/// `make:listener` — an event listener class in `app/listeners/`.
fn generate_listener(name: &str, event: Option<&str>) -> (String, String) {
    let base = to_snake_case(name);
    let class_name = to_pascal_case(&base);
    let path = format!("app/listeners/{base}.ts");

    // `--event` binds the listener to a generated event class.
    let (import_line, event_type, registration) = match event {
        Some(event_name) => {
            let event_base = to_snake_case(event_name);
            let event_class = to_pascal_case(&event_base);
            (
                format!("import type {event_class} from '#app/events/{event_base}.js'\n\n"),
                event_class.clone(),
                format!("emitter.on({event_class}, {class_name})"),
            )
        }
        None => (
            String::new(),
            "unknown".to_string(),
            format!("emitter.on(SomeEvent, {class_name})"),
        ),
    };

    let content = format!(
        r#"{import_line}/**
 * Register it with the emitter:
 *   {registration}
 */
export default class {class_name} {{
  async handle(event: {event_type}): Promise<void> {{
    // React to the event.
    void event
  }}
}}
"#
    );
    (path, content)
}

/// Drop a trailing `Middleware` so `make:middleware AuthMiddleware` does not
/// produce `AuthMiddlewareMiddleware`.
fn strip_suffix_ci(name: &str, suffix: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let suffix_lower = suffix.to_ascii_lowercase();
    if lower.len() > suffix_lower.len() && lower.ends_with(&suffix_lower) {
        let cut = name.len() - suffix.len();
        return name[..cut].trim_end_matches(['_', '-']).to_string();
    }
    name.to_string()
}

const MIGRATION: &str = r#"import { Migration } from '@c9up/atlas'

export default class {{ className }} extends Migration {
  up() {
    this.schema.createTable('TABLE_NAME', (t) => {
      t.increments('id') // or: t.uuid('id').primary() for app-generated UUIDs
      t.timestamps()
    })
  }

  down() {
    this.schema.dropTable('TABLE_NAME')
  }
}
"#;

/// A migration file, emitted as part of `make:module`.
///
/// Not publishable and not resolved against a stub: `make:migration` belongs to
/// the data package, which knows where migrations live and what one imports.
fn generate_migration(name: &str) -> Result<(String, String), String> {
    let date = today_yyyymmdd()?;
    let seq = next_migration_sequence(&date);
    let snake = to_snake_case(name);
    let path = format!("database/migrations/{date}{seq:03}_{snake}.ts");
    let vars = vars_of([("className", to_pascal_case(name))]);
    Ok((path, crate::stubs::render(MIGRATION, &vars)))
}

const SEEDER: &str = r#"import { Seeder } from '@c9up/atlas'

/** Seeds data for the `{{ module }}` module. */
export default class {{ className }} extends Seeder {
  async run() {
    // Insert seed data here
  }
}
"#;

fn seeder_template(module: &str, name: &str) -> Template {
    let class_name = ensure_suffix(name, "Seeder");
    Template {
        path: format!("database/seeders/{class_name}.ts"),
        body: SEEDER,
        vars: vars_of([
            ("className", class_name),
            ("module", module.to_string()),
            ("name", name.to_string()),
        ]),
    }
}

fn generate_seeder(module: &str, name: &str) -> Result<(String, String), String> {
    seeder_template(module, name).resolve("seeder")
}

fn today_yyyymmdd() -> Result<String, String> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("System clock error: {e}"))?;
    let days = now.as_secs() / 86_400;
    let (year, month, day) = days_to_date(days);
    Ok(format!("{year:04}{month:02}{day:02}"))
}

/// Next per-day migration sequence: scans `database/migrations` for existing
/// `<date><NNN>_*.ts` files and returns max + 1 (1-based). Filenames are
/// `YYYYMMDDNNN_name.ts`, so they sort chronologically and never collide.
fn next_migration_sequence(date: &str) -> u32 {
    let mut max = 0u32;
    if let Ok(entries) = std::fs::read_dir("database/migrations") {
        for entry in entries.flatten() {
            let fname = entry.file_name();
            let fname = fname.to_string_lossy();
            if let Some(rest) = fname.strip_prefix(date) {
                let digits: String = rest.chars().take(3).collect();
                if digits.len() == 3 {
                    if let Ok(n) = digits.parse::<u32>() {
                        max = max.max(n);
                    }
                }
            }
        }
    }
    max + 1
}

fn days_to_date(days: u64) -> (u64, u64, u64) {
    let mut y = 1970u64;
    let mut remaining = days;
    loop {
        let year_days = if is_leap(y) { 366 } else { 365 };
        if remaining < year_days {
            break;
        }
        remaining -= year_days;
        y += 1;
    }
    let months = [
        31,
        if is_leap(y) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut m = 1u64;
    for &md in &months {
        if remaining < md {
            break;
        }
        remaining -= md;
        m += 1;
    }
    (y, m, remaining + 1)
}

fn is_leap(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

#[cfg(test)]
mod generator_conventions {
    use super::*;

    /// Adonis documents a `Middleware` suffix for middleware ("names are
    /// singular with a 'middleware' suffix, e.g. BodyParserMiddleware") but no
    /// suffix for events or listeners. Getting this backwards produced
    /// `OrderShippedEventEvent`-shaped names, so it is pinned here.
    #[test]
    fn generated_names_follow_adonis_suffix_rules() {
        let (path, content) = generate_middleware("auth", "router").expect("generator");
        assert_eq!(path, "app/middleware/auth_middleware.ts");
        assert!(content.contains("class AuthMiddleware"), "{content}");

        let (path, content) = generate_event("orderShipped").expect("generator");
        assert_eq!(path, "app/events/order_shipped.ts");
        assert!(
            content.contains("class OrderShipped extends BaseEvent"),
            "{content}"
        );
        assert!(
            !content.contains("OrderShippedEvent"),
            "no Event suffix: {content}"
        );

        let (path, content) = generate_listener("sendMail", None);
        assert_eq!(path, "app/listeners/send_mail.ts");
        assert!(content.contains("class SendMail"), "{content}");
        assert!(
            !content.contains("SendMailListener"),
            "no Listener suffix: {content}"
        );
    }

    /// `--stack` picks the registration hint, `--event` types the handler.
    #[test]
    fn generator_options_change_the_output() {
        let (_, server) = generate_middleware("auth", "server").expect("generator");
        assert!(server.contains("server.use("), "{server}");
        let (_, named) = generate_middleware("auth", "named").expect("generator");
        assert!(named.contains("router.named("), "{named}");

        let (_, listener) = generate_listener("sendMail", Some("orderShipped"));
        assert!(listener.contains("import type OrderShipped"), "{listener}");
        assert!(
            listener.contains("handle(event: OrderShipped)"),
            "{listener}"
        );
        assert!(
            listener.contains("emitter.on(OrderShipped, SendMail)"),
            "{listener}"
        );
    }

    /// A namespaced command name is a valid COMMAND name but not a valid class
    /// or file name — `app:provision` must not leak a colon into either.
    #[test]
    fn namespaced_command_names_are_sanitised() {
        assert!(validate_command_name("app:provision").is_ok());
        assert!(validate_command_name(":bad").is_err());
        assert!(validate_command_name("a::b").is_err());

        let (path, content) = generate_command("app:provision").expect("generator");
        assert_eq!(path, "commands/app-provision.ts");
        assert!(content.contains("class AppProvision"), "{content}");
        assert!(
            content.contains("commandName = 'app:provision'"),
            "{content}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_class_name_rejects_injection() {
        assert!(validate_class_name("Orders; rm -rf /", "name").is_err());
        assert!(validate_class_name("../etc", "name").is_err());
        assert!(validate_class_name("", "name").is_err());
        assert!(validate_class_name("orders", "name").is_err()); // lowercase first letter
        assert!(validate_class_name("Order Item", "name").is_err());
        assert!(validate_class_name("Order-Item", "name").is_err());
        assert!(validate_class_name("Order_Item", "name").is_err());
    }

    #[test]
    fn validate_class_name_accepts_pascal() {
        assert!(validate_class_name("Order", "name").is_ok());
        assert!(validate_class_name("OrderItem", "name").is_ok());
        assert!(validate_class_name("UsersV2", "name").is_ok());
    }

    #[test]
    fn validate_module_name_kebab_only() {
        assert!(validate_module_name("orders", "module").is_ok());
        assert!(validate_module_name("user-profiles", "module").is_ok());
        assert!(validate_module_name("v2-orders", "module").is_ok());
        assert!(validate_module_name("Orders", "module").is_err()); // pascal
        assert!(validate_module_name("user_profiles", "module").is_err()); // snake
        assert!(validate_module_name("../evil", "module").is_err());
        assert!(validate_module_name("", "module").is_err());
    }

    #[test]
    fn plan_or_write_dry_does_not_touch_disk() {
        // Use a path that is virtually guaranteed not to exist in any cwd.
        let mut plan = Plan::default();
        let outcome =
            plan_or_write(&mut plan, true, false, "app/orders/Order.ts", "// content").unwrap();
        assert!(matches!(outcome, WriteOutcome::Created(_)));
        assert_eq!(plan.files.len(), 1);
        assert_eq!(plan.files[0].path, "app/orders/Order.ts");
        assert_eq!(plan.files[0].content, "// content");
    }

    #[test]
    fn plan_or_write_rejects_path_traversal() {
        let mut plan = Plan::default();
        let err = plan_or_write(&mut plan, true, false, "../etc/passwd", "evil").unwrap_err();
        assert!(err.contains("outside project root"));
    }

    #[test]
    fn make_module_umbrella_emits_four_files() {
        // Build the plan via the same helpers `make_module` uses, without
        // routing through stdout/cwd side-effects.
        let mut plan = Plan::default();
        for (path, content) in [
            generate_entity("orders", "Order").expect("entity"),
            generate_controller("orders", "Order").expect("controller"),
            generate_validator("orders", "Order").expect("validator"),
            generate_migration("Order").expect("migration"),
        ] {
            plan_or_write(&mut plan, true, false, &path, &content).unwrap();
        }
        assert_eq!(plan.files.len(), 4);
        assert_eq!(plan.files[0].path, "app/orders/Order.ts");
        assert_eq!(plan.files[1].path, "app/orders/OrderController.ts");
        assert_eq!(plan.files[2].path, "app/orders/OrderValidator.ts");
        assert!(plan.files[3].path.starts_with("database/migrations/"));
    }

    #[test]
    fn validate_name_relaxed_rejects_leading_dash_or_underscore() {
        assert!(validate_name_relaxed("-evil", "name").is_err());
        assert!(validate_name_relaxed("_hidden", "name").is_err());
        assert!(validate_name_relaxed("add_users_table", "name").is_ok());
        assert!(validate_name_relaxed("CreateOrders", "name").is_ok());
        assert!(validate_name_relaxed("ok-name", "name").is_ok());
    }

    /// A generated file carries nothing from this repository's own bookkeeping.
    ///
    /// Every template used to open with `@implements FR<auto-assigned> — TODO
    /// describe`, a traceability marker for a tracking system the application
    /// being scaffolded does not have and cannot fill in.
    #[test]
    fn a_generated_file_carries_no_marker_from_this_repository() {
        for kind in crate::stubs::PUBLISHABLE {
            let stub = built_in_stub(kind).expect("template");
            assert!(
                !stub.contains("@implements"),
                "{kind} still carries a traceability marker: {stub}"
            );
        }
        let migration = generate_migration("CreateOrders").expect("generator").1;
        assert!(!migration.contains("@implements"), "{migration}");
    }

    #[test]
    fn migration_filename_is_date_plus_sequence() {
        // Filename shape: `YYYYMMDDNNN_<snake>.ts` — 8-digit date + 3-digit
        // per-day sequence (the counter advances only as files land on disk,
        // so two calls without writing legitimately yield the same path).
        let (path, _) = generate_migration("CreateOrders").unwrap();
        let prefix = "database/migrations/";
        assert!(path.starts_with(prefix));
        let stem = path.trim_start_matches(prefix);
        let underscore = stem.find('_').expect("must contain `_`");
        let date_seq = &stem[..underscore];
        assert_eq!(
            date_seq.len(),
            8 + 3,
            "date+sequence should be 11 digits, got `{date_seq}`"
        );
        assert!(date_seq.chars().all(|c| c.is_ascii_digit()));
        assert!(stem.ends_with("_create_orders.ts"));
    }

    #[test]
    fn seeder_works_without_module() {
        // Module is optional for seeders — they live under
        // database/seeders/, not app/<m>/. Empty module must not error.
        let entries = build_plan_entries("seeder", "", "User").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "database/seeders/UserSeeder.ts");
    }
}

/// The built-in template for `kind` — the text itself, not a reconstruction.
///
/// A published stub is EXACTLY what the generator renders, because it is the
/// same string: `stubs:publish` writes this, and the generator substitutes it.
/// Rebuilt instead by running the generator with sentinel names and swapping
/// each sentinel back out, two variables that rendered to the same sentinel
/// were indistinguishable, and the published copy quietly disagreed with the
/// built-in output — see {@link Template}.
pub fn built_in_stub(kind: &str) -> Option<String> {
    // Any arguments will do: the body is the same text whatever it is rendered
    // with. `""` for a module keeps the call sites uniform.
    let template = match kind {
        "controller" => controller_template("", ""),
        "entity" => entity_template("", ""),
        "service" => service_template("", ""),
        "validator" => validator_template("", ""),
        "seeder" => seeder_template("", ""),
        "provider" => provider_template(""),
        "command" => command_template(""),
        "middleware" => middleware_template("", "router"),
        "event" => event_template(""),
        _ => return None,
    };
    Some(template.body.to_string())
}

/// The `{{ … }}` names `kind`'s template substitutes.
///
/// Read off the template, so `stubs:publish --list` names exactly what a stub
/// author can use — no second list to keep in step.
pub fn stub_variables(kind: &str) -> Option<Vec<&'static str>> {
    let template = match kind {
        "controller" => controller_template("", ""),
        "entity" => entity_template("", ""),
        "service" => service_template("", ""),
        "validator" => validator_template("", ""),
        "seeder" => seeder_template("", ""),
        "provider" => provider_template(""),
        "command" => command_template(""),
        "middleware" => middleware_template("", "router"),
        "event" => event_template(""),
        _ => return None,
    };
    Some(template.vars.into_keys().collect())
}

#[cfg(test)]
mod stub_publish_tests {
    use super::*;
    use crate::stubs::PUBLISHABLE;

    #[test]
    fn every_publishable_kind_has_a_built_in_template() {
        for kind in PUBLISHABLE {
            assert!(
                built_in_stub(kind).is_some(),
                "stubs::PUBLISHABLE lists '{kind}' but no template renders for it"
            );
        }
    }

    /// Every kind, not just the one that happened to be checked.
    ///
    /// Publishing a stub must not change what the generator emits. Checked on
    /// `controller` alone, two kinds were silently wrong: `command` published
    /// `commandName = '{{ className }}'` (so `make:command app:provision`
    /// generated `commandName = 'AppProvision'`), and `middleware` published a
    /// registration hint pointing at `{{ name }}_middleware.js` — the raw
    /// argument — while the file is written from its snake_case stem.
    #[test]
    fn a_published_stub_renders_back_to_the_built_in_output() {
        // Names chosen so a variable standing in for another is visible: the
        // class, the file stem and the name the user typed all differ.
        let cases: Vec<(&str, (String, String), Template)> = vec![
            (
                "controller",
                generate_controller("billing", "Invoice").expect("generator"),
                controller_template("billing", "Invoice"),
            ),
            (
                "entity",
                generate_entity("billing", "Invoice").expect("generator"),
                entity_template("billing", "Invoice"),
            ),
            (
                "service",
                generate_service("billing", "Payment").expect("generator"),
                service_template("billing", "Payment"),
            ),
            (
                "validator",
                generate_validator("billing", "CreateInvoice").expect("generator"),
                validator_template("billing", "CreateInvoice"),
            ),
            (
                "seeder",
                generate_seeder("billing", "Invoice").expect("generator"),
                seeder_template("billing", "Invoice"),
            ),
            (
                "provider",
                generate_provider("Stripe").expect("generator"),
                provider_template("Stripe"),
            ),
            (
                "command",
                generate_command("app:provision").expect("generator"),
                command_template("app:provision"),
            ),
            (
                "middleware",
                generate_middleware("AuthCheck", "named").expect("generator"),
                middleware_template("AuthCheck", "named"),
            ),
            (
                "event",
                generate_event("orderShipped").expect("generator"),
                event_template("orderShipped"),
            ),
        ];

        for (kind, (_, direct), template) in cases {
            let stub = built_in_stub(kind).expect("template");
            assert_eq!(
                crate::stubs::render(&stub, &template.vars),
                direct,
                "{kind}: the published stub does not render back to what the generator emits"
            );
        }
    }

    /// A published stub keeps the meaning of the name it was given.
    ///
    /// `make:command app:provision` has to declare `app:provision`, not the
    /// class name derived from it: the console kernel resolves the command by
    /// that string, and so does the scan that lets an app override a built-in.
    #[test]
    fn a_published_command_stub_declares_the_name_that_was_asked_for() {
        let stub = built_in_stub("command").expect("template");
        assert!(
            stub.contains("commandName = '{{ name }}'"),
            "the command stub must declare {{{{ name }}}}: {stub}"
        );

        let (_, content) = generate_command("app:provision").expect("generator");
        assert!(
            content.contains("commandName = 'app:provision'"),
            "{content}"
        );
        assert!(content.contains("class AppProvision"), "{content}");
    }

    /// The registration hint has to be copy-pasteable.
    ///
    /// `--stack named` emitted `router.named({{ auth: … }})` — a `format!`
    /// escape in a plain string literal, which reached the file as doubled
    /// braces — and pointed at the raw argument rather than the file that was
    /// written.
    #[test]
    fn the_middleware_hint_names_the_file_that_was_written() {
        for (stack, expected) in [
            ("router", "router.use([() => import('#middleware/auth_check_middleware.js')])"),
            ("server", "server.use([() => import('#middleware/auth_check_middleware.js')])"),
            (
                "named",
                "router.named({ auth_check: () => import('#middleware/auth_check_middleware.js') })",
            ),
        ] {
            let (path, content) = generate_middleware("AuthCheck", stack).expect("generator");
            assert_eq!(path, "app/middleware/auth_check_middleware.ts");
            assert!(content.contains(expected), "--stack {stack}: {content}");
            assert!(!content.contains("{{"), "--stack {stack} left a brace escape: {content}");
        }
    }

    /// A stub author can name every variable the generator fills, and nothing
    /// the generator does not.
    ///
    /// One direction only: a variable the built-in body does not mention is
    /// still worth exposing — `module` is what a `to:` front matter redirects
    /// on. The other direction is the failure that reaches a file.
    #[test]
    fn a_published_stub_has_no_placeholder_nobody_fills() {
        for kind in PUBLISHABLE {
            let stub = built_in_stub(kind).expect("template");
            let known = stub_variables(kind).expect("variables");
            let mut rest = stub.as_str();
            while let Some(start) = rest.find("{{") {
                let after = &rest[start + 2..];
                let end = after.find("}}").expect("a closed placeholder");
                let name = after[..end].trim();
                assert!(
                    known.contains(&name),
                    "{kind}: `{{{{ {name} }}}}` is in the template but nothing fills it"
                );
                rest = &after[end + 2..];
            }
        }
    }
}
