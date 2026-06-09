//! Generator — code generation from templates (pure Rust, no Node.js).
//!
//! Story 33.4 reshape: every generator now plans first, then either writes
//! the plan to disk or emits it as JSON for the MCP `dryRun` path.

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
            return Err(format!(
                "refusing to write through a symlink: {path}"
            ));
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
    eprintln!("  \x1b[32m{}\x1b[0m {path}", if exists { "modified" } else { "created" });
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
        _ => {}
    }

    let entries = build_plan_entries(kind, module, name)?;
    flush_outcome(entries, dry_run, force)
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
        generate_entity(module, name),
        generate_controller(module, name),
        generate_validator(module, name),
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
        return Err(format!(
            "{label} '{s}' must not start with '-' or '_'"
        ));
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

fn build_plan_entries(
    kind: &str,
    module: &str,
    name: &str,
) -> Result<Vec<(String, String)>, String> {
    let entry = match kind {
        "service" => {
            require_module(module, kind)?;
            generate_service(module, name)
        }
        "entity" => {
            require_module(module, kind)?;
            generate_entity(module, name)
        }
        "controller" => {
            require_module(module, kind)?;
            generate_controller(module, name)
        }
        "validator" => {
            require_module(module, kind)?;
            generate_validator(module, name)
        }
        "provider" => generate_provider(name),
        "migration" => generate_migration(name)?,
        "seeder" => {
            // Module is OPTIONAL for seeders — they live under
            // `database/seeders/`, not `app/<module>/`. When provided,
            // the module name is stamped into the seeder JSDoc for
            // traceability; when omitted we just emit the file.
            generate_seeder(module, name)
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

fn flush_outcome(
    entries: Vec<(String, String)>,
    dry_run: bool,
    force: bool,
) -> Result<(), String> {
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
                    eprintln!(
                        "  \x1b[33mrollback\x1b[0m: failed to restore {path}: {e}"
                    );
                } else {
                    eprintln!("  \x1b[33mrollback\x1b[0m: restored {path}");
                }
            }
            None => {
                if let Err(e) = fs::remove_file(path) {
                    eprintln!(
                        "  \x1b[33mrollback\x1b[0m: failed to remove {path}: {e}"
                    );
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

fn generate_service(module: &str, name: &str) -> (String, String) {
    let class_name = ensure_suffix(name, "Service");
    let path = format!("app/{module}/{class_name}.ts");
    let content = format!(
        r#"import {{ Service }} from '@c9up/ream'

/** @implements FR<auto-assigned> — TODO describe */
@Service()
export class {class_name} {{
  async findAll() {{
    return []
  }}

  async findById(id: string) {{
    return null
  }}

  async create(data: Record<string, unknown>) {{
    return data
  }}

  async update(id: string, data: Record<string, unknown>) {{
    return {{ id, ...data }}
  }}

  async delete(id: string) {{
    return {{ id }}
  }}
}}
"#
    );
    (path, content)
}

fn generate_entity(module: &str, name: &str) -> (String, String) {
    let table_name = format!("{}s", to_snake_case(name));
    let path = format!("app/{module}/{name}.ts");
    let content = format!(
        r#"import {{ Entity, Column, PrimaryKey, BaseEntity }} from '@c9up/atlas'

/** @implements FR<auto-assigned> — TODO describe */
@Entity('{table_name}')
export class {name} extends BaseEntity {{
  @PrimaryKey() id!: string
  @Column() createdAt!: string
  @Column() updatedAt!: string
}}
"#
    );
    (path, content)
}

fn generate_controller(module: &str, name: &str) -> (String, String) {
    let class_name = ensure_suffix(name, "Controller");
    let path = format!("app/{module}/{class_name}.ts");
    let content = format!(
        r#"import type {{ HttpContext }} from '@c9up/ream'

/** @implements FR<auto-assigned> — TODO describe */
export class {class_name} {{
  async index({{ response }}: HttpContext) {{
    response.status(200).json([])
  }}

  async show({{ params, response }}: HttpContext) {{
    const {{ id }} = params
    response.status(200).json({{ id }})
  }}

  async store({{ request, response }}: HttpContext) {{
    const data = request.body()
    response.status(201).json({{ created: true }})
  }}

  async update({{ params, response }}: HttpContext) {{
    const {{ id }} = params
    response.status(200).json({{ id, updated: true }})
  }}

  async destroy({{ params, response }}: HttpContext) {{
    const {{ id }} = params
    response.status(204).send('')
  }}
}}
"#
    );
    (path, content)
}

fn generate_validator(module: &str, name: &str) -> (String, String) {
    let class_name = ensure_suffix(name, "Validator");
    let path = format!("app/{module}/{class_name}.ts");
    let content = format!(
        r#"import {{ rules, schema }} from '@c9up/rune'

/** @implements FR<auto-assigned> — TODO describe */
export const {class_name} = schema({{
  // Define validation rules
  // name: rules.string().min(1).max(255),
  // email: rules.string().email(),
}})
"#
    );
    (path, content)
}

fn generate_provider(name: &str) -> (String, String) {
    let class_name = ensure_suffix(name, "Provider");
    let path = format!("providers/{class_name}.ts");
    let content = format!(
        r#"import {{ Provider }} from '@c9up/ream'

/** @implements FR<auto-assigned> — TODO describe */
export default class {class_name} extends Provider {{
  register() {{
    // Register bindings in the container
  }}

  async boot() {{
    // Connect and verify
  }}

  async start() {{
    // Runs before HTTP server starts
  }}

  async ready() {{
    // Application operational
  }}

  async shutdown() {{
    // Cleanup
  }}
}}
"#
    );
    (path, content)
}

fn generate_migration(name: &str) -> Result<(String, String), String> {
    let date = today_yyyymmdd()?;
    let seq = next_migration_sequence(&date);
    let snake = to_snake_case(name);
    let path = format!("database/migrations/{date}{seq:03}_{snake}.ts");
    let class_name = to_pascal_case(name);
    let content = format!(
        r#"import {{ Migration }} from '@c9up/atlas'

/** @implements FR<auto-assigned> — TODO describe */
export default class {class_name} extends Migration {{
  up() {{
    this.schema.createTable('TABLE_NAME', (t) => {{
      t.increments('id') // or: t.uuid('id').primary() for app-generated UUIDs
      t.timestamps()
    }})
  }}

  down() {{
    this.schema.dropTable('TABLE_NAME')
  }}
}}
"#
    );
    Ok((path, content))
}

fn generate_seeder(module: &str, name: &str) -> (String, String) {
    let class_name = ensure_suffix(name, "Seeder");
    let path = format!("database/seeders/{class_name}.ts");
    let content = format!(
        r#"import {{ Seeder }} from '@c9up/atlas'

/**
 * Seeds data for the `{module}` module.
 * @implements FR<auto-assigned> — TODO describe
 */
export default class {class_name} extends Seeder {{
  async run() {{
    // Insert seed data here
  }}
}}
"#
    );
    (path, content)
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
        let outcome = plan_or_write(
            &mut plan,
            true,
            false,
            "app/orders/Order.ts",
            "// content",
        )
        .unwrap();
        assert!(matches!(outcome, WriteOutcome::Created(_)));
        assert_eq!(plan.files.len(), 1);
        assert_eq!(plan.files[0].path, "app/orders/Order.ts");
        assert_eq!(plan.files[0].content, "// content");
    }

    #[test]
    fn plan_or_write_rejects_path_traversal() {
        let mut plan = Plan::default();
        let err = plan_or_write(
            &mut plan,
            true,
            false,
            "../etc/passwd",
            "evil",
        )
        .unwrap_err();
        assert!(err.contains("outside project root"));
    }

    #[test]
    fn make_module_umbrella_emits_four_files() {
        // Build the plan via the same helpers `make_module` uses, without
        // routing through stdout/cwd side-effects.
        let mut plan = Plan::default();
        for (path, content) in [
            generate_entity("orders", "Order"),
            generate_controller("orders", "Order"),
            generate_validator("orders", "Order"),
            generate_migration("Order").unwrap(),
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

    #[test]
    fn fr_implements_marker_baked_into_every_template() {
        // Story 33.2's traceability tools key off `@implements FR` —
        // every generated file must carry the marker.
        let entity = generate_entity("orders", "Order").1;
        let controller = generate_controller("orders", "Order").1;
        let validator = generate_validator("orders", "Order").1;
        let provider = generate_provider("App").1;
        let migration = generate_migration("CreateOrders").unwrap().1;
        let seeder = generate_seeder("orders", "User").1;
        let service = generate_service("orders", "Mailer").1;
        for body in [&entity, &controller, &validator, &provider, &migration, &seeder, &service] {
            assert!(body.contains("@implements FR"), "missing FR marker in template: {body}");
        }
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
