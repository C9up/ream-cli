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

fn project_root() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or_else(|| ".".to_string())
}

pub fn install() -> Result<(), String> {
    if !Path::new("package.json").exists() {
        return Err("Not in a Ream project (no package.json found)".to_string());
    }
    let mut root = load()?;
    let obj = root
        .as_object_mut()
        .ok_or_else(|| format!("{CONFIG} must be a JSON object"))?;
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| format!("{CONFIG} \"mcpServers\" must be a JSON object"))?;

    let existed = servers.contains_key(SERVER_KEY);
    servers.insert(
        SERVER_KEY.to_string(),
        json!({
            "command": "npx",
            "args": ["-y", PACKAGE],
            "env": { "REAM_PROJECT_ROOT": project_root() }
        }),
    );
    save(&root)?;

    println!();
    if existed {
        crate::ui::success(&format!("updated '{SERVER_KEY}' in {CONFIG}"));
    } else {
        crate::ui::success(&format!(
            "registered '{SERVER_KEY}' ({PACKAGE}) in {CONFIG}"
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
