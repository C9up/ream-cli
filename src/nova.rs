//! Nova subcommands — VAPID key generation.

use std::path::Path;
use std::process::{Command, Stdio};

const VAPID_SCRIPT: &str = r#"
import { generateVapidKeys } from '@c9up/nova';
const k = generateVapidKeys();
process.stdout.write(JSON.stringify(k));
"#;

const SUBJECT_DEFAULT: &str = "mailto:noreply@example.com";

#[derive(Debug, PartialEq)]
struct VapidKeyPair {
    public: String,
    private: String,
}

pub fn run_vapid_generate(force: bool) -> Result<(), String> {
    if !Path::new("package.json").exists() {
        return Err("Not in a Ream project (no package.json found)".to_string());
    }

    let env_path = Path::new(".env");
    let existing = if env_path.exists() {
        std::fs::read_to_string(env_path).map_err(|e| format!("Failed to read .env: {}", e))?
    } else {
        String::new()
    };

    if !force {
        if let Some(value) = read_env_value(&existing, "NOVA_VAPID_PRIVATE_KEY") {
            if !value.is_empty() {
                return Err("NOVA_VAPID_PRIVATE_KEY is already set in .env.\n  \
                     Re-run with --force to overwrite, or `unset NOVA_VAPID_PRIVATE_KEY` first."
                    .to_string());
            }
        }
    }

    let pair = invoke_node()?;

    let mut updated = upsert_env_var(&existing, "NOVA_VAPID_PUBLIC_KEY", &pair.public);
    updated = upsert_env_var(&updated, "NOVA_VAPID_PRIVATE_KEY", &pair.private);
    if read_env_value(&updated, "NOVA_VAPID_SUBJECT").is_none() {
        updated = upsert_env_var(&updated, "NOVA_VAPID_SUBJECT", SUBJECT_DEFAULT);
    }

    std::fs::write(env_path, updated).map_err(|e| format!("Failed to write .env: {}", e))?;

    println!();
    println!("  \x1b[32mGenerated VAPID key pair\x1b[0m");
    println!("  NOVA_VAPID_PUBLIC_KEY  = {}", pair.public);
    // Public key is safe to echo (it's literally meant to be served to
    // browsers); the private key never goes to stdout — it would land in
    // shell history, scrollback, CI logs, screen recordings and
    // third-party wrappers. The .env write is the only authorised sink.
    println!(
        "  NOVA_VAPID_PRIVATE_KEY = [redacted — written to .env, {} chars]",
        pair.private.chars().count()
    );
    println!(
        "  NOVA_VAPID_SUBJECT     = {} (edit this!)",
        SUBJECT_DEFAULT
    );
    println!();
    println!("  Wrote .env. Move private key to a secrets manager before deploying.");
    println!();

    Ok(())
}

fn invoke_node() -> Result<VapidKeyPair, String> {
    let output = Command::new("node")
        .args([
            "--import",
            "@swc-node/register/esm-register",
            "--input-type=module",
            "-e",
            VAPID_SCRIPT,
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("Failed to spawn node: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "Node exited with code {} while running generateVapidKeys()",
            output.status.code().unwrap_or(-1)
        ));
    }

    let raw = String::from_utf8(output.stdout)
        .map_err(|e| format!("Node stdout was not valid UTF-8: {}", e))?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Node printed no JSON (is @c9up/nova installed?)".to_string());
    }

    let parsed: serde_json::Value =
        serde_json::from_str(trimmed).map_err(|e| format!("Failed to parse VAPID JSON: {}", e))?;
    let public = parsed
        .get("publicKey")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing 'publicKey' in JSON output".to_string())?;
    let private = parsed
        .get("privateKey")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing 'privateKey' in JSON output".to_string())?;
    Ok(VapidKeyPair {
        public: public.to_string(),
        private: private.to_string(),
    })
}

/// Read an env var's value from a `.env` file's raw text. Strips matching
/// surrounding quotes; ignores lines that start with `#`.
fn read_env_value(content: &str, key: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if let Some((k, v)) = trimmed.split_once('=') {
            if k.trim() == key {
                return Some(strip_quotes(v).to_string());
            }
        }
    }
    None
}

/// Upsert `KEY=VALUE` in a `.env` file's raw text. Preserves all other lines
/// verbatim. If `KEY` is already present, replaces its line in place;
/// otherwise appends a new line at the end (with a trailing newline).
fn upsert_env_var(content: &str, key: &str, value: &str) -> String {
    let new_line = format!("{}={}", key, value);
    let mut out = String::with_capacity(content.len() + new_line.len() + 1);
    let mut replaced = false;
    let mut last_was_newline = false;
    for line in content.split_inclusive('\n') {
        let stripped = line.strip_suffix('\n').unwrap_or(line);
        let trimmed = stripped.trim_start();
        let key_match = !trimmed.starts_with('#')
            && trimmed
                .split_once('=')
                .map(|(k, _)| k.trim() == key)
                .unwrap_or(false);
        if key_match && !replaced {
            out.push_str(&new_line);
            if line.ends_with('\n') {
                out.push('\n');
                last_was_newline = true;
            } else {
                last_was_newline = false;
            }
            replaced = true;
        } else {
            out.push_str(line);
            last_was_newline = line.ends_with('\n');
        }
    }
    if !replaced {
        if !out.is_empty() && !last_was_newline {
            out.push('\n');
        }
        out.push_str(&new_line);
        out.push('\n');
    }
    out
}

fn strip_quotes(value: &str) -> &str {
    let trimmed = value.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2)
        || (trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2)
    {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_env_value_returns_existing_value() {
        let env = "FOO=bar\nNOVA_VAPID_PRIVATE_KEY=abc123\n# comment\n";
        assert_eq!(
            read_env_value(env, "NOVA_VAPID_PRIVATE_KEY"),
            Some("abc123".to_string())
        );
    }

    #[test]
    fn read_env_value_returns_none_when_missing() {
        let env = "FOO=bar\n";
        assert_eq!(read_env_value(env, "NOVA_VAPID_PRIVATE_KEY"), None);
    }

    #[test]
    fn read_env_value_strips_double_quotes() {
        let env = "NOVA_VAPID_PRIVATE_KEY=\"with-quotes\"\n";
        assert_eq!(
            read_env_value(env, "NOVA_VAPID_PRIVATE_KEY"),
            Some("with-quotes".to_string())
        );
    }

    #[test]
    fn read_env_value_skips_comment_lines_with_matching_key() {
        let env = "# NOVA_VAPID_PRIVATE_KEY=should-skip\nNOVA_VAPID_PRIVATE_KEY=real\n";
        assert_eq!(
            read_env_value(env, "NOVA_VAPID_PRIVATE_KEY"),
            Some("real".to_string())
        );
    }

    #[test]
    fn read_env_value_returns_empty_string_for_blank_value() {
        let env = "NOVA_VAPID_PRIVATE_KEY=\n";
        assert_eq!(
            read_env_value(env, "NOVA_VAPID_PRIVATE_KEY"),
            Some(String::new())
        );
    }

    #[test]
    fn upsert_env_var_appends_when_missing() {
        let env = "FOO=bar\n";
        let out = upsert_env_var(env, "NOVA_VAPID_PUBLIC_KEY", "pub");
        assert_eq!(out, "FOO=bar\nNOVA_VAPID_PUBLIC_KEY=pub\n");
    }

    #[test]
    fn upsert_env_var_replaces_existing_in_place_preserving_neighbours() {
        let env = "FOO=bar\nNOVA_VAPID_PUBLIC_KEY=old\nBAZ=qux\n";
        let out = upsert_env_var(env, "NOVA_VAPID_PUBLIC_KEY", "new");
        assert_eq!(out, "FOO=bar\nNOVA_VAPID_PUBLIC_KEY=new\nBAZ=qux\n");
    }

    #[test]
    fn upsert_env_var_creates_file_content_from_empty_input() {
        let out = upsert_env_var("", "NOVA_VAPID_PUBLIC_KEY", "pub");
        assert_eq!(out, "NOVA_VAPID_PUBLIC_KEY=pub\n");
    }

    #[test]
    fn upsert_env_var_adds_trailing_newline_before_appending() {
        let env = "FOO=bar";
        let out = upsert_env_var(env, "NOVA_VAPID_PUBLIC_KEY", "pub");
        assert_eq!(out, "FOO=bar\nNOVA_VAPID_PUBLIC_KEY=pub\n");
    }

    #[test]
    fn upsert_env_var_does_not_clobber_commented_key() {
        let env = "# NOVA_VAPID_PUBLIC_KEY=do-not-touch\n";
        let out = upsert_env_var(env, "NOVA_VAPID_PUBLIC_KEY", "real");
        assert_eq!(
            out,
            "# NOVA_VAPID_PUBLIC_KEY=do-not-touch\nNOVA_VAPID_PUBLIC_KEY=real\n"
        );
    }
}
