//! Nebula subcommands — copy components out of the registry, and list it.
//!
//! Same shape as `nova.rs`: the work lives in the package, and this dispatches
//! to it through Node. `@c9up/nebula` owns `registry.json` and knows where it
//! is installed; neither is something the CLI can work out, and duplicating
//! either here would put the registry format in two repos with separate
//! release cycles.
//!
//! Component names reach the script as a JSON literal, never spliced into its
//! text. A name is user input, and `"; process.exit(1); //` interpolated into
//! a program is arbitrary code — the same reason `codemods.rs` encodes its
//! flags rather than formatting them in.

use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

// Both scripts answer in an envelope rather than throwing. An unknown
// component name is a user mistake with a good message already attached — let
// it escape as an exception and Node prints a stack trace, on top of which
// this side can only guess at the cause and usually guesses "the package is
// not installed", which is exactly wrong. A genuinely missing package still
// fails at `import`, before the handler, and that IS the install error.
const ADD_SCRIPT: &str = r#"
import { add } from '@c9up/nebula/cli';
const options = JSON.parse(OPTIONS_LITERAL);
try {
  process.stdout.write(JSON.stringify({ ok: true, value: add({ cwd: process.cwd(), ...options }) }));
} catch (error) {
  const message = error instanceof Error ? error.message : String(error);
  process.stdout.write(JSON.stringify({ ok: false, message }));
}
"#;

const LIST_SCRIPT: &str = r#"
import { join } from 'node:path';
import { loadRegistry, packageRoot } from '@c9up/nebula/cli';
try {
  const root = packageRoot(join(process.cwd(), 'package.json'));
  process.stdout.write(JSON.stringify({ ok: true, value: loadRegistry(join(root, 'registry.json')) }));
} catch (error) {
  const message = error instanceof Error ? error.message : String(error);
  process.stdout.write(JSON.stringify({ ok: false, message }));
}
"#;

/// `ream nebula:add <component…>`
pub fn run_add(
    components: &[String],
    force: bool,
    dry_run: bool,
    language: Option<&str>,
) -> Result<(), String> {
    require_project()?;
    if components.is_empty() {
        return Err(
            "Name at least one component — `ream nebula:add button`.\n  \
             `ream nebula:list` shows what the registry holds."
                .to_string(),
        );
    }

    let mut options = json!({
        "names": components,
        "force": force,
        "dryRun": dry_run,
    });
    if let Some(value) = language {
        options["language"] = Value::String(value.to_string());
    }

    let result = invoke_node(ADD_SCRIPT, Some(&options))?;
    let written = string_list(&result, "written");
    let skipped = string_list(&result, "skipped");
    let language = result
        .get("language")
        .and_then(Value::as_str)
        .unwrap_or("javascript");

    println!();
    for path in &written {
        println!("  \x1b[32mcreate\x1b[0m  {}", path);
    }
    for path in &skipped {
        println!("  exists  {}", path);
    }
    if !skipped.is_empty() {
        println!();
        println!("  Existing files were left alone. Re-run with --force to overwrite.");
    }
    println!();
    // Stated rather than assumed: the language is inferred from the tree in
    // most runs, and copying the wrong one writes files the project cannot
    // load — a silent failure worth one line to prevent.
    println!(
        "  Copied as {}. Override with --ts or --js.",
        if language == "ts" {
            "TypeScript"
        } else {
            "JavaScript"
        }
    );
    println!();
    Ok(())
}

/// `ream nebula:list`
pub fn run_list(layer: Option<&str>) -> Result<(), String> {
    require_project()?;
    let registry = invoke_node(LIST_SCRIPT, None)?;
    let items = registry
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| "registry.json has no items array".to_string())?;

    for name in ["atoms", "molecules", "organisms", "templates"] {
        if layer.is_some_and(|only| only != name) {
            continue;
        }
        let in_layer: Vec<&str> = items
            .iter()
            .filter(|item| item.get("layer").and_then(Value::as_str) == Some(name))
            .filter_map(|item| item.get("name").and_then(Value::as_str))
            .collect();
        if in_layer.is_empty() {
            continue;
        }
        println!();
        println!("  \x1b[1m{}\x1b[0m ({})", name, in_layer.len());
        println!("  {}", in_layer.join(", "));
    }
    println!();
    Ok(())
}

fn require_project() -> Result<(), String> {
    if Path::new("package.json").exists() {
        return Ok(());
    }
    Err("Not in a Ream project (no package.json found)".to_string())
}

/// Run a script against the installed package and parse what it printed.
///
/// `options` is embedded as a JSON *literal* — encoded twice, so the value the
/// script parses is a string containing JSON rather than program text. That is
/// what keeps a component name out of the executable surface.
fn invoke_node(script: &str, options: Option<&Value>) -> Result<Value, String> {
    let source = match options {
        None => script.to_string(),
        Some(value) => {
            let json = serde_json::to_string(value)
                .map_err(|e| format!("Failed to encode options: {}", e))?;
            let literal = serde_json::to_string(&json)
                .map_err(|e| format!("Failed to encode options literal: {}", e))?;
            script.replace("OPTIONS_LITERAL", &literal)
        }
    };

    let output = Command::new("node")
        .args([
            "--import",
            "@swc-node/register/esm-register",
            "--input-type=module",
            "-e",
            &source,
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("Failed to spawn node: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "Node exited with code {} — is @c9up/nebula installed? \
             (`ream add @c9up/nebula`)",
            output.status.code().unwrap_or(-1)
        ));
    }

    let raw = String::from_utf8(output.stdout)
        .map_err(|e| format!("Node stdout was not valid UTF-8: {}", e))?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Node printed nothing (is @c9up/nebula installed?)".to_string());
    }

    let envelope: Value = serde_json::from_str(trimmed)
        .map_err(|e| format!("Failed to parse nebula output: {}", e))?;
    if envelope.get("ok").and_then(Value::as_bool) == Some(true) {
        return envelope
            .get("value")
            .cloned()
            .ok_or_else(|| "nebula answered ok with no value".to_string());
    }
    // The package's own message, verbatim — it knows what went wrong and says
    // so better than anything reconstructed here.
    Err(envelope
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("nebula reported an unspecified failure")
        .to_string())
}

fn string_list(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_are_embedded_as_data_not_as_program_text() {
        // A component name is user input. Spliced into the script it would be
        // executable; encoded twice it is a string literal the script parses.
        //
        // The payload's *text* is of course still in the source — that is what
        // embedding data means. What matters is that the quote which would
        // close the literal and start a new statement is escaped, so the whole
        // thing survives a round trip as one string.
        let hostile = json!({ "names": ["\"; process.exit(1); //"] });
        let json = serde_json::to_string(&hostile).unwrap();
        let literal = serde_json::to_string(&json).unwrap();
        let source = ADD_SCRIPT.replace("OPTIONS_LITERAL", &literal);

        // The literal in the source parses back to exactly what went in.
        let recovered: String = serde_json::from_str(&literal).unwrap();
        let parsed: Value = serde_json::from_str(&recovered).unwrap();
        assert_eq!(parsed, hostile);

        // Every quote from the payload is escaped, so none of them can close
        // the literal. A `contains` check cannot say this — the payload's text
        // is present either way; what distinguishes data from code is the
        // backslash in front of the quote.
        let needle = "\"; process.exit";
        let mut at = 0;
        let mut found = 0;
        while let Some(index) = source[at..].find(needle) {
            let absolute = at + index;
            assert!(
                absolute > 0 && source.as_bytes()[absolute - 1] == b'\\',
                "an unescaped quote at byte {absolute} would close the literal"
            );
            found += 1;
            at = absolute + needle.len();
        }
        assert_eq!(found, 1, "the payload should appear exactly once");
    }

    #[test]
    fn a_missing_key_reads_as_an_empty_list() {
        assert!(string_list(&json!({}), "written").is_empty());
        assert_eq!(
            string_list(&json!({ "written": ["a", "b"] }), "written"),
            vec!["a".to_string(), "b".to_string()]
        );
    }
}
