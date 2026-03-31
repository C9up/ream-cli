//! Generator — code generation from templates (pure Rust, no Node.js).

use std::fs;
use std::path::Path;

/// Generate a file based on type, module, and name.
pub fn make(kind: &str, module: &str, name: &str) -> Result<(), String> {
    let (path, content) = match kind {
        "service" => generate_service(module, name),
        "entity" => generate_entity(module, name),
        "controller" => generate_controller(module, name),
        "validator" => generate_validator(module, name),
        "provider" => generate_provider(name),
        "migration" => generate_migration(name),
        _ => return Err(format!("Unknown generator type: {}", kind)),
    };

    let full_path = Path::new(&path);
    if let Some(parent) = full_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create directory: {}", e))?;
    }

    fs::write(full_path, content).map_err(|e| format!("Failed to write file: {}", e))?;
    println!("  \x1b[32mcreated\x1b[0m {}", path);
    Ok(())
}

fn ensure_suffix(name: &str, suffix: &str) -> String {
    if name.ends_with(suffix) { name.to_string() } else { format!("{}{}", name, suffix) }
}

fn to_snake_case(name: &str) -> String {
    let mut result = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 { result.push('_'); }
            result.push(c.to_lowercase().next().unwrap());
        } else {
            result.push(c);
        }
    }
    result
}

fn generate_service(module: &str, name: &str) -> (String, String) {
    let class_name = ensure_suffix(name, "Service");
    let path = format!("app/modules/{}/services/{}.ts", module, class_name);
    let content = format!(r#"import {{ Service }} from '@c9up/ream'

@Service()
export class {} {{
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
"#, class_name);
    (path, content)
}

fn generate_entity(module: &str, name: &str) -> (String, String) {
    let table_name = format!("{}s", to_snake_case(name));
    let path = format!("app/modules/{}/entities/{}.ts", module, name);
    let content = format!(r#"import {{ Entity, Column, PrimaryKey, BaseEntity }} from '@c9up/atlas'

@Entity('{}')
export class {} extends BaseEntity {{
  @PrimaryKey() id!: string
  @Column() createdAt!: string
  @Column() updatedAt!: string
}}
"#, table_name, name);
    (path, content)
}

fn generate_controller(module: &str, name: &str) -> (String, String) {
    let class_name = ensure_suffix(name, "Controller");
    let path = format!("app/modules/{}/controllers/{}.ts", module, class_name);
    let content = format!(r#"export class {} {{
  async index(ctx: {{ response: {{ status: number; headers: Record<string, string>; body: string }} }}) {{
    ctx.response.headers['content-type'] = 'application/json'
    ctx.response.body = JSON.stringify([])
  }}

  async show(ctx: {{ params: Record<string, string>; response: {{ status: number; headers: Record<string, string>; body: string }} }}) {{
    const {{ id }} = ctx.params
    ctx.response.headers['content-type'] = 'application/json'
    ctx.response.body = JSON.stringify({{ id }})
  }}

  async store(ctx: {{ request: {{ body: string }}; response: {{ status: number; headers: Record<string, string>; body: string }} }}) {{
    ctx.response.status = 201
    ctx.response.headers['content-type'] = 'application/json'
    ctx.response.body = JSON.stringify({{ created: true }})
  }}

  async update(ctx: {{ params: Record<string, string>; response: {{ status: number; headers: Record<string, string>; body: string }} }}) {{
    const {{ id }} = ctx.params
    ctx.response.headers['content-type'] = 'application/json'
    ctx.response.body = JSON.stringify({{ id, updated: true }})
  }}

  async destroy(ctx: {{ params: Record<string, string>; response: {{ status: number; headers: Record<string, string>; body: string }} }}) {{
    const {{ id }} = ctx.params
    ctx.response.status = 204
    ctx.response.body = ''
  }}
}}
"#, class_name);
    (path, content)
}

fn generate_validator(module: &str, name: &str) -> (String, String) {
    let class_name = ensure_suffix(name, "Validator");
    let path = format!("app/modules/{}/validators/{}.ts", module, class_name);
    let content = format!(r#"import {{ rules, schema }} from '@c9up/rune'

export const {} = schema({{
  // Define validation rules
  // name: rules.string().min(1).max(255),
  // email: rules.string().email(),
}})
"#, class_name);
    (path, content)
}

fn generate_provider(name: &str) -> (String, String) {
    let class_name = ensure_suffix(name, "Provider");
    let path = format!("providers/{}.ts", class_name);
    let content = format!(r#"import {{ Provider }} from '@c9up/ream'

export default class {} extends Provider {{
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
"#, class_name);
    (path, content)
}

fn generate_migration(name: &str) -> (String, String) {
    let timestamp = chrono_timestamp();
    let snake = to_snake_case(name);
    let path = format!("database/migrations/{}_{}.ts", timestamp, snake);
    let content = r#"export async function up() {
  // TODO: implement migration
}

export async function down() {
  // TODO: implement rollback
}
"#.to_string();
    (path, content)
}

fn chrono_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    // Simple timestamp: YYYYMMDDHHmmss
    let s = secs;
    let sec = s % 60; let s = s / 60;
    let min = s % 60; let s = s / 60;
    let hour = s % 24; let days = s / 24;
    // Approximate date from days since epoch
    let (year, month, day) = days_to_date(days);
    format!("{:04}{:02}{:02}{:02}{:02}{:02}", year, month, day, hour, min, sec)
}

fn days_to_date(days: u64) -> (u64, u64, u64) {
    // Simplified date calculation from days since 1970-01-01
    let mut y = 1970u64;
    let mut remaining = days;
    loop {
        let year_days = if is_leap(y) { 366 } else { 365 };
        if remaining < year_days { break; }
        remaining -= year_days;
        y += 1;
    }
    let months = [31, if is_leap(y) { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut m = 1u64;
    for &md in &months {
        if remaining < md { break; }
        remaining -= md;
        m += 1;
    }
    (y, m, remaining + 1)
}

fn is_leap(y: u64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}
