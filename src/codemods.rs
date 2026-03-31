//! Codemods — modify project files programmatically (pure Rust).

use std::fs;
use std::path::Path;

/// Configure a package — modify reamrc.ts, .env, generate config files.
pub fn configure(package: &str, force: bool) -> Result<(), String> {
    // Validate package name — must match @scope/name or plain name pattern
    let is_valid_npm_name = |s: &str| -> bool {
        // @scope/name or plain name — no .. or path separators beyond single /
        if s.contains("..") { return false; }
        if s.starts_with('@') {
            let parts: Vec<&str> = s.splitn(2, '/').collect();
            parts.len() == 2
                && parts[0].len() > 1
                && parts[1].chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
        } else {
            s.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        }
    };
    if !is_valid_npm_name(package) {
        return Err(format!("Invalid package name: {}", package));
    }

    println!("\n  Configuring {}...\n", package);

    match package {
        "@c9up/atlas" => configure_atlas(force),
        "@c9up/warden" => configure_warden(force),
        "@c9up/spectrum" => configure_spectrum(force),
        "@c9up/pulsar" => configure_pulsar(force),
        "@c9up/tailwind" => configure_tailwind(force),
        "@c9up/photon" => configure_photon(force),
        _ => Err(format!("No configure hook for '{}'. Package must provide a configure hook.", package)),
    }?;

    println!("\n  \x1b[32mDone!\x1b[0m {} configured.\n", package);
    Ok(())
}

fn configure_atlas(force: bool) -> Result<(), String> {
    add_provider_to_reamrc("#providers/AtlasProvider.js")?;
    add_env_vars(&[("DB_CONNECTION", "postgres"), ("DB_HOST", "localhost"), ("DB_PORT", "5432"), ("DB_DATABASE", "ream"), ("DB_USER", "postgres"), ("DB_PASSWORD", "secret")])?;
    write_config_file("config/atlas.ts", r#"import { defineConfig, env } from '@c9up/ream'

export default defineConfig({
  connection: env('DB_CONNECTION', 'postgres'),
  connections: {
    postgres: {
      host: env('DB_HOST', 'localhost'),
      port: Number(env('DB_PORT', '5432')),
      database: env('DB_DATABASE', 'ream'),
    },
  },
})
"#, force)
}

fn configure_warden(force: bool) -> Result<(), String> {
    add_provider_to_reamrc("#providers/AuthProvider.js")?;
    add_env_vars(&[("JWT_SECRET", "change-me-to-a-random-32-byte-secret"), ("JWT_EXPIRY", "3600")])?;
    write_config_file("config/warden.ts", r#"import { defineConfig, env } from '@c9up/ream'

export default defineConfig({
  defaultStrategy: 'jwt',
  jwt: {
    secret: env('JWT_SECRET'),
    expiry: Number(env('JWT_EXPIRY', '3600')),
  },
})
"#, force)
}

fn configure_spectrum(force: bool) -> Result<(), String> {
    add_provider_to_reamrc("#providers/LogProvider.js")?;
    write_config_file("config/spectrum.ts", r#"import { defineConfig, env } from '@c9up/ream'

export default defineConfig({
  level: env('LOG_LEVEL', 'info'),
  channels: ['console'],
})
"#, force)
}

fn configure_pulsar(force: bool) -> Result<(), String> {
    add_provider_to_reamrc("#providers/BusProvider.js")?;
    write_config_file("config/pulsar.ts", r#"import { defineConfig } from '@c9up/ream'

export default defineConfig({
  store: 'memory',
  retries: 3,
})
"#, force)
}

fn configure_tailwind(force: bool) -> Result<(), String> {
    write_config_file("tailwind.config.ts", r#"/** @type {import('tailwindcss').Config} */
export default {
  content: ['./app/**/*.{ts,tsx,vue,svelte}', './resources/**/*.{html,ts,tsx}'],
  theme: { extend: {} },
  plugins: [],
}
"#, force)?;
    write_config_file("postcss.config.js", r#"export default {
  plugins: {
    tailwindcss: {},
    autoprefixer: {},
  },
}
"#, force)?;
    write_config_file("resources/css/app.css", "@tailwind base;\n@tailwind components;\n@tailwind utilities;\n", force)
}

fn configure_photon(force: bool) -> Result<(), String> {
    add_provider_to_reamrc("#providers/PhotonProvider.js")?;
    write_config_file("config/photon.ts", r#"import { defineConfig } from '@c9up/ream'

export default defineConfig({
  framework: 'react',
  entryClient: 'resources/app.tsx',
  entryServer: 'resources/ssr.tsx',
})
"#, force)
}

fn add_provider_to_reamrc(import_path: &str) -> Result<(), String> {
    let rc_path = "reamrc.ts";
    if !Path::new(rc_path).exists() {
        println!("  \x1b[33mreamrc.ts not found — skipping provider registration\x1b[0m");
        return Ok(());
    }

    let content = fs::read_to_string(rc_path).map_err(|e| e.to_string())?;
    if content.contains(import_path) {
        return Ok(());
    }

    let entry = format!("    () => import('{}'),", import_path);
    let updated = content.replace(
        "providers: [",
        &format!("providers: [\n{}", entry),
    );

    fs::write(rc_path, updated).map_err(|e| e.to_string())?;
    println!("  \x1b[32mupdated\x1b[0m reamrc.ts — added {}", import_path);
    Ok(())
}

fn add_env_vars(vars: &[(&str, &str)]) -> Result<(), String> {
    let env_path = ".env";
    let mut content = if Path::new(env_path).exists() {
        fs::read_to_string(env_path).map_err(|e| e.to_string())?
    } else {
        String::new()
    };

    let mut added = 0;
    for (key, value) in vars {
        let pattern = format!("\n{}=", key);
        let start_pattern = format!("{}=", key);
        if !content.contains(&pattern) && !content.starts_with(&start_pattern) {
            content.push_str(&format!("{}={}\n", key, value));
            added += 1;
        }
    }

    if added > 0 {
        fs::write(env_path, &content).map_err(|e| e.to_string())?;
        println!("  \x1b[32mupdated\x1b[0m .env — {} variable(s)", added);
    }
    Ok(())
}

fn write_config_file(path: &str, content: &str, force: bool) -> Result<(), String> {
    let full = Path::new(path);

    // Path traversal guard — reject any parent directory components
    for component in Path::new(path).components() {
        if matches!(component, std::path::Component::ParentDir) {
            return Err(format!("Refusing to write outside project root: {}", path));
        }
    }

    if full.exists() && !force {
        println!("  \x1b[33mskipped\x1b[0m {} (already exists, use --force)", path);
        return Ok(());
    }

    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(full, content).map_err(|e| e.to_string())?;
    println!("  \x1b[32mcreated\x1b[0m {}", path);
    Ok(())
}
