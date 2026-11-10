//! `ream mcp install|uninstall|status` — manage the `@c9up/ream-mcp` server
//! registration in the project's `.mcp.json`, the config Claude Code, Cursor,
//! and other agents read to launch stdio MCP servers.
//!
//! Merges are non-destructive: other `mcpServers` entries are preserved
//! (serde_json `preserve_order` keeps the file's key order stable).

use serde_json::{json, Map, Value};
use std::fs;
use std::path::Path;

const CONFIG: &str = ".mcp.json";
const SERVER_KEY: &str = "ream";
const PACKAGE: &str = "@c9up/ream-mcp";

fn load() -> Result<Value, String> {
    if !Path::new(CONFIG).exists() {
        return Ok(json!({}));
    }
    let raw = fs::read_to_string(CONFIG).map_err(|e| format!("Failed to read {CONFIG}: {e}"))?;
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&raw).map_err(|e| format!("{CONFIG} is not valid JSON: {e}"))
}

fn save(v: &Value) -> Result<(), String> {
    let mut s = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
    s.push('\n');
    fs::write(CONFIG, s).map_err(|e| format!("Failed to write {CONFIG}: {e}"))
}

/// The server entry, identical on every machine: `.mcp.json` is committed.
///
/// No `REAM_PROJECT_ROOT`: the server finds the root from the directory the
/// client starts it in, and an absolute path would carry one developer's home
/// into the repository and point everyone else's server at nothing. `npx --no`
/// runs the project's own `@c9up/ream-mcp`, the version its lockfile pins, and
/// refuses to download one — `-y` fetched whatever was latest at each start.
fn server_entry() -> Value {
    json!({
        "command": "npx",
        "args": ["--no", PACKAGE],
    })
}

/// Put the entry into `root`, keeping every other server. `true` when it
/// replaced an earlier one.
fn register(root: &mut Value) -> Result<bool, String> {
    let obj = root
        .as_object_mut()
        .ok_or_else(|| format!("{CONFIG} must be a JSON object"))?;
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| format!("{CONFIG} \"mcpServers\" must be a JSON object"))?;
    Ok(servers
        .insert(SERVER_KEY.to_string(), server_entry())
        .is_some())
}

/// Does `package.json` declare the server, in any dependency block?
fn declares_server(manifest: &str) -> bool {
    let Ok(parsed) = serde_json::from_str::<Value>(manifest) else {
        return false;
    };
    ["dependencies", "devDependencies"].iter().any(|block| {
        parsed
            .get(block)
            .and_then(|deps| deps.get(PACKAGE))
            .is_some()
    })
}

pub fn install() -> Result<(), String> {
    if !Path::new("package.json").exists() {
        return Err("Not in a Ream project (no package.json found)".to_string());
    }
    let mut root = load()?;
    let existed = register(&mut root)?;
    save(&root)?;

    println!();
    if existed {
        crate::ui::success(&format!("updated '{SERVER_KEY}' in {CONFIG}"));
    } else {
        crate::ui::success(&format!(
            "registered '{SERVER_KEY}' ({PACKAGE}) in {CONFIG}"
        ));
    }
    let declared = fs::read_to_string("package.json")
        .map(|manifest| declares_server(&manifest))
        .unwrap_or(false);
    if !declared {
        crate::ui::warning(&format!(
            "{PACKAGE} is not a dependency of this project, and the entry never downloads it: run `ream add -D {PACKAGE}`"
        ));
    }
    crate::ui::info("restart your MCP client (Claude Code / Cursor / …) to pick it up");
    println!();
    Ok(())
}

pub fn uninstall() -> Result<(), String> {
    if !Path::new(CONFIG).exists() {
        println!();
        crate::ui::info(&format!("{CONFIG} not found — nothing to remove"));
        println!();
        return Ok(());
    }
    let mut root = load()?;
    let removed = root
        .as_object_mut()
        .and_then(|o| o.get_mut("mcpServers"))
        .and_then(|s| s.as_object_mut())
        .map(|servers| servers.remove(SERVER_KEY).is_some())
        .unwrap_or(false);

    println!();
    if removed {
        save(&root)?;
        crate::ui::success(&format!("removed '{SERVER_KEY}' from {CONFIG}"));
    } else {
        crate::ui::info(&format!(
            "'{SERVER_KEY}' was not registered in {CONFIG} — nothing to do"
        ));
    }
    println!();
    Ok(())
}

pub fn status() -> Result<(), String> {
    let root = load()?;
    let entry = root.get("mcpServers").and_then(|s| s.get(SERVER_KEY));
    match entry {
        Some(e) => {
            println!();
            crate::ui::success(&format!("ream MCP is installed in {CONFIG}"));
            if let Some(dir) = e
                .get("env")
                .and_then(|env| env.get("REAM_PROJECT_ROOT"))
                .and_then(|v| v.as_str())
            {
                println!(
                    "    {} {dir}",
                    crate::ui::paint("REAM_PROJECT_ROOT:", crate::ui::DIM)
                );
            }
            // An entry written by an earlier `install`: a machine path and an
            // unpinned download. Reported, not rewritten — status changes nothing.
            if e != &server_entry() {
                crate::ui::warning(
                    "the entry differs from the portable one — run `ream mcp install` to rewrite it",
                );
            }
            println!();
        }
        None => {
            println!();
            crate::ui::warning("ream MCP is not installed — run `ream mcp install`");
            println!();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_names_no_machine_path_and_never_downloads() {
        let mut root = json!({});
        register(&mut root).unwrap();
        let entry = &root["mcpServers"]["ream"];
        assert_eq!(
            entry,
            &json!({ "command": "npx", "args": ["--no", "@c9up/ream-mcp"] })
        );
        assert!(entry.get("env").is_none());
        let rendered = root.to_string();
        let cwd = std::env::current_dir().unwrap();
        assert!(!rendered.contains(&*cwd.to_string_lossy()), "{rendered}");
        assert!(!rendered.contains("\"-y\""), "{rendered}");
    }

    #[test]
    fn other_servers_are_kept_and_an_old_entry_is_replaced_whole() {
        let mut root = json!({
            "mcpServers": {
                "other": { "command": "node", "args": ["server.js"] },
                "ream": {
                    "command": "npx",
                    "args": ["-y", "@c9up/ream-mcp"],
                    "env": { "REAM_PROJECT_ROOT": "/home/someone/work/app" }
                }
            },
            "unrelated": true
        });
        assert!(register(&mut root).unwrap());
        assert_eq!(
            root["mcpServers"]["other"],
            json!({ "command": "node", "args": ["server.js"] })
        );
        assert_eq!(root["mcpServers"]["ream"], server_entry());
        assert_eq!(root["unrelated"], json!(true));
        let keys: Vec<&String> = root["mcpServers"].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["other", "ream"]);
    }

    #[test]
    fn a_non_object_config_is_refused() {
        assert!(register(&mut json!([])).is_err());
        assert!(register(&mut json!({ "mcpServers": [] })).is_err());
    }

    #[test]
    fn the_server_dependency_is_found_in_either_block() {
        assert!(declares_server(
            r#"{"devDependencies":{"@c9up/ream-mcp":"^0.1.23"}}"#
        ));
        assert!(declares_server(
            r#"{"dependencies":{"@c9up/ream-mcp":"0.1.23"}}"#
        ));
        assert!(!declares_server(
            r#"{"devDependencies":{"@c9up/ream":"^0.2.0"}}"#
        ));
        assert!(!declares_server("not json"));
    }
}
