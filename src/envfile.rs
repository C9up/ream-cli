//! Reading and writing single keys in a `.env` file.
//!
//! `generate:key` writes `APP_KEY` there. The rules are the fussy part — a
//! commented-out line must not be treated as the key, an existing value is
//! replaced in place so neighbouring lines and comments survive, and an append
//! never runs two entries together on one line.

/// Read an env var's value from a `.env` file's raw text. Strips matching
/// surrounding quotes; ignores lines that start with `#`.
pub(crate) fn read_env_value(content: &str, key: &str) -> Option<String> {
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
pub(crate) fn upsert_env_var(content: &str, key: &str, value: &str) -> String {
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
