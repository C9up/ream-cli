//! `ream template <name> [destination]` — install a project template by cloning
//! the corresponding upstream repo (shallow), stripping its `.git`, initialising a
//! fresh one, and patching `package.json` so the resulting project is ready to go.
//!
//! Unlike `scaffold` (which writes a small set of inline files for `api`/`web`/
//! `slim`/`microservice`), `template` pulls full reference apps from their own
//! repositories — so a heavy demo like `kitchen-sink` stays maintained next to
//! the framework instead of being duplicated into the CLI source.
//!
//! Registry is kept tiny on purpose: an unknown template name fails loudly with
//! the list of supported ones, no silent fallback to an attacker-controllable URL.
//!
//! Pre-flight checks:
//!   - destination must not exist (no silent clobber)
//!   - `git` must be on PATH
//!
//! Post-clone:
//!   - remove the upstream `.git`
//!   - `git init -b main` so the user starts with a fresh, empty history
//!   - rewrite `package.json` `name` to match the destination dir name
//!     (preserves key order via serde_json's `preserve_order` feature)

use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::Command;

/// Returns the upstream git URL for a known template, or `None` for unknown ones.
fn template_registry(name: &str) -> Option<&'static str> {
    match name {
        "kitchen-sink" => Some("https://github.com/C9up/kitchen-sink.git"),
        _ => None,
    }
}

/// Human-readable list of every template the registry knows about — kept in sync
/// with `template_registry()` so an unknown name surfaces a helpful suggestion.
fn known_templates() -> &'static [&'static str] {
    &["kitchen-sink"]
}

pub fn run(name: &str, destination: Option<&str>) -> Result<(), String> {
    // Default destination = template name (mirrors `npm create <pkg>` ergonomics).
    let dest = destination.unwrap_or(name);
    let url = template_registry(name).ok_or_else(|| {
        format!(
            "Unknown template '{}'. Available: {}",
            name,
            known_templates().join(", ")
        )
    })?;

    let dest_path = Path::new(dest);
    if dest_path.exists() {
        return Err(format!(
            "Destination '{}' already exists — refusing to clobber.",
            dest
        ));
    }

    println!("  Cloning {} into {} …", url, dest);
    let status = Command::new("git")
        .args(["clone", "--depth=1", url, dest])
        .status()
        .map_err(|e| format!("`git clone` failed: {}. Is git on PATH?", e))?;
    if !status.success() {
        return Err(format!(
            "`git clone` exited with code {}",
            status.code().unwrap_or(-1)
        ));
    }

    // Drop the upstream history so the user gets a clean slate.
    let upstream_git = dest_path.join(".git");
    fs::remove_dir_all(&upstream_git)
        .map_err(|e| format!("Failed to remove cloned .git: {}", e))?;

    let status = Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dest_path)
        .status()
        .map_err(|e| format!("`git init` failed: {}", e))?;
    if !status.success() {
        return Err("`git init` failed".to_string());
    }

    // Patch package.json so the new project's name matches its directory.
    // serde_json with `preserve_order` keeps the original key ordering — no
    // unsolicited reshuffling of the dependencies block.
    let pkg_path = dest_path.join("package.json");
    if pkg_path.exists() {
        let raw = fs::read_to_string(&pkg_path)
            .map_err(|e| format!("Failed to read package.json: {}", e))?;
        let mut pkg: Value = serde_json::from_str(&raw)
            .map_err(|e| format!("package.json is not valid JSON: {}", e))?;
        let new_name = dest_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(dest)
            .to_string();
        if let Some(obj) = pkg.as_object_mut() {
            obj.insert("name".to_string(), Value::String(new_name));
        }
        let updated = serde_json::to_string_pretty(&pkg)
            .map_err(|e| format!("Failed to serialize package.json: {}", e))?;
        // Preserve trailing newline convention.
        fs::write(&pkg_path, format!("{}\n", updated))
            .map_err(|e| format!("Failed to write package.json: {}", e))?;
    }

    println!();
    println!("  ✓ Template '{}' installed in '{}'", name, dest);
    println!();
    println!("  Next steps:");
    println!("    cd {}", dest);
    println!("    pnpm install");
    println!("    ream migrate");
    println!("    pnpm dev");
    Ok(())
}
