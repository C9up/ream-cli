//! Codemods — dynamic configure dispatch via Node.js.

use std::time::Duration;

use serde_json::{Map, Value};
use wait_timeout::ChildExt;

/// Timeout for the existence-check probe — bounded because the probe is just
/// `await import(spec)` which fails or returns sub-second normally; the
/// timeout is a safety net against module-level deadlocks (top-level `await`
/// stuck on a dead network, init-time `while(true)` loops).
///
/// Default 10s; override via `REAM_PROBE_TIMEOUT_SECS=<n>` (n > 0). Note: the
/// configure run itself is intentionally UNBOUNDED — legitimate configure work
/// (long file writes, interactive prompts, migrations) can take arbitrarily
/// long, and Ctrl-C remains the canonical escape there.
fn probe_timeout() -> Duration {
    parse_probe_timeout(std::env::var("REAM_PROBE_TIMEOUT_SECS").ok().as_deref())
}

/// Pure parser split out from `probe_timeout()` so unit tests can exercise the
/// parsing rules without racing the global process env.
fn parse_probe_timeout(raw: Option<&str>) -> Duration {
    let secs = raw
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(10);
    Duration::from_secs(secs)
}

/// Detect the "loader missing" failure mode in the existence-check stderr.
///
/// `configure_with_flags` spawns Node with `--import @swc-node/register/esm-register`
/// because source-first packages export `.ts` files directly. If
/// the loader is missing from the user's `node_modules`, Node's own resolver
/// emits one of these patterns BEFORE the inline probe script runs. Anything
/// else is a real import error inside the user's package and surfaces verbatim.
///
/// Pattern coverage:
///   - `Cannot find package '@swc-node/register'` (Node stock resolver)
///   - `ERR_MODULE_NOT_FOUND` paired with `@swc-node/register` in the message
///   - `--import` flag rejected when register can't be loaded
fn is_loader_missing(stderr: &str) -> bool {
    stderr.contains("@swc-node/register")
        && (stderr.contains("Cannot find package")
            || stderr.contains("Cannot find module")
            || stderr.contains("ERR_MODULE_NOT_FOUND")
            || stderr.contains("MODULE_NOT_FOUND"))
}

/// Outcome of a configure() dispatch.
///
/// `ream add` downgrades `NoHook` to a stderr note + exit 0 (the install
/// already succeeded; the absence of a hook is a property of the package, not
/// a runtime failure). `ream configure` keeps the historic behaviour of
/// raising an error in that case (the maintainer asked for "configure" — a
/// missing hook is the explicit failure mode for that command).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigureOutcome {
    Configured,
    NoHook,
}

/// Validate an npm package name.
///
/// Rejects: `..` substrings, post-`/` segments that start with `.` or `_`
/// (npm policy — would otherwise resolve oddly through `import()`), and any
/// char outside `[a-zA-Z0-9._-]` in the name part. `pub(crate)` so `add.rs`
/// can share the single source of truth.
pub(crate) fn is_valid_npm_name(s: &str) -> bool {
    if s.is_empty() || s.contains("..") {
        return false;
    }
    if s.starts_with('@') {
        let parts: Vec<&str> = s.splitn(2, '/').collect();
        if parts.len() != 2 || parts[0].len() <= 1 || parts[1].is_empty() {
            return false;
        }
        // npm rejects names whose post-`/` segment starts with `.` or `_`.
        let first = parts[1].chars().next().unwrap_or('\0');
        if first == '.' || first == '_' {
            return false;
        }
        parts[1]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    } else {
        let first = s.chars().next().unwrap_or('\0');
        if first == '.' || first == '_' {
            return false;
        }
        s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    }
}

/// Flags forwarded to a configure() hook, in the order typed. `None` is a
/// bare `--flag`.
pub type FlagPairs = Vec<(String, Vec<Option<String>>)>;

/// The flags as `Record<string, string[]>`, the hook's second argument since
/// before it received a command object. A bare flag is `"true"` there.
fn raw_flags(flags: &[(String, Vec<Option<String>>)]) -> Value {
    let mut obj = Map::new();
    for (key, values) in flags {
        let arr = values
            .iter()
            .map(|value| Value::String(value.clone().unwrap_or_else(|| "true".to_string())))
            .collect();
        obj.insert(key.clone(), Value::Array(arr));
    }
    Value::Object(obj)
}

/// yargs-parser's number test, which the console's flag parser runs with
/// `parse-numbers` on: hex, or a decimal without a leading zero.
fn looks_numeric(value: &str) -> Option<f64> {
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        if !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return u64::from_str_radix(hex, 16).ok().map(|n| n as f64);
        }
        return None;
    }
    let bytes = value.as_bytes();
    if bytes.len() > 1 && bytes[0] == b'0' && bytes[1] != b'.' {
        return None;
    }
    let digits = value.strip_prefix('-').unwrap_or(value);
    let (mantissa, exponent) = match digits.find(['e', 'E']) {
        Some(at) => (&digits[..at], Some(&digits[at + 1..])),
        None => (digits, None),
    };
    let (int, frac) = match mantissa.split_once('.') {
        Some((int, frac)) => (int, Some(frac)),
        None => (mantissa, None),
    };
    let int_ok = int.chars().all(|c| c.is_ascii_digit());
    let frac_ok = frac.is_none_or(|f| f.chars().all(|c| c.is_ascii_digit()));
    // `\d+(\.\d*)?` or `\.\d+`
    let shape_ok = (!int.is_empty() || frac.is_some_and(|f| !f.is_empty())) && int_ok && frac_ok;
    let exponent_ok = exponent.is_none_or(|e| {
        let e = e.strip_prefix(['+', '-']).unwrap_or(e);
        !e.is_empty() && e.chars().all(|c| c.is_ascii_digit())
    });
    if !shape_ok || !exponent_ok {
        return None;
    }
    let number: f64 = value.parse().ok()?;
    // Past the safe-integer range a number would no longer be the value typed.
    (number.is_finite() && number.floor().abs() <= 9_007_199_254_740_991.0).then_some(number)
}

fn flag_value(value: &Option<String>) -> Value {
    match value {
        None => Value::Bool(true),
        Some(text) => match looks_numeric(text).and_then(serde_json::Number::from_f64) {
            Some(number) => {
                // `3`, not `3.0`: an integral value stays an integer in JSON.
                let integral = number.as_f64().filter(|n| n.fract() == 0.0);
                match integral {
                    Some(n) => Value::from(n as i64),
                    None => Value::Number(number),
                }
            }
            None => Value::String(text.clone()),
        },
    }
}

/// The flags as the console parses them into `command.parsedFlags`: a bare
/// flag is `true`, `--no-x` is `x: false`, a numeric value is a number, a
/// repeated flag is an array, and `force` is there when it was passed.
fn parsed_flags(flags: &[(String, Vec<Option<String>>)], force: bool) -> Value {
    let mut obj = Map::new();
    for (key, values) in flags {
        let (key, values): (String, Vec<Value>) = match key.strip_prefix("no-") {
            Some(negated) if values.iter().all(Option::is_none) => {
                (negated.to_string(), vec![Value::Bool(false); values.len()])
            }
            _ => (key.clone(), values.iter().map(flag_value).collect()),
        };
        let merged = match obj.remove(&key) {
            None if values.len() == 1 => values.into_iter().next().unwrap_or(Value::Null),
            None => Value::Array(values),
            Some(Value::Array(mut existing)) => {
                existing.extend(values);
                Value::Array(existing)
            }
            Some(existing) => {
                let mut all = vec![existing];
                all.extend(values);
                Value::Array(all)
            }
        };
        obj.insert(key, merged);
    }
    if force {
        obj.insert("force".to_string(), Value::Bool(true));
    }
    Value::Object(obj)
}

/// The first line of the configure script: what the host needs, as one JSON
/// value inside a JS string literal, so no part of it is ever read as code.
fn configure_input(
    package: &str,
    subpath: &str,
    force: bool,
    flags: &[(String, Vec<Option<String>>)],
) -> Result<String, String> {
    let input = serde_json::json!({
        "name": package,
        "sub": subpath,
        "root": package,
        "force": force,
        "flags": raw_flags(flags),
        "parsedFlags": parsed_flags(flags, force),
    });
    let json = serde_json::to_string(&input)
        .map_err(|e| format!("Failed to encode the configure input: {e}"))?;
    let literal = serde_json::to_string(&json)
        .map_err(|e| format!("Failed to encode the configure input: {e}"))?;
    Ok(format!("const INPUT = JSON.parse({literal});\n"))
}

/// The script that runs the hook; see the file for the command object it builds.
const CONFIGURE_HOST: &str = include_str!("configure_host.mjs");

/// Configure a package with optional flags forwarded to its `configure()`
/// hook, which receives a command object (prompt, parsedFlags, logger,
/// createCodemods…) as its first argument and the raw flags as its second.
///
/// Returns `Ok(NoHook)` instead of erroring when the package exists but does
/// not export a configure() function — callers (notably `ream add`) decide
/// whether that case is fatal.
pub fn configure_with_flags(
    package: &str,
    force: bool,
    flags: &[(String, Vec<Option<String>>)],
) -> Result<ConfigureOutcome, String> {
    if !is_valid_npm_name(package) {
        return Err(format!("Invalid package name: {}", package));
    }

    // Prefer the lightweight ./configure subpath (no native binary load).
    // Fall back to root entrypoint if the subpath doesn't exist.
    let configure_subpath = format!("{}/configure", package);

    // The package name is encoded into the inline script as a JS string
    // literal via `serde_json::to_string` (whose output IS a valid JS string
    // literal — JSON strings are a syntactic subset of JS string literals).
    // Belt-and-suspenders to keep the JS template immune to validator
    // regressions (e.g. relaxing for version specifiers); the embedded
    // identifier never participates in JS-source parsing as code.
    let sub_literal = serde_json::to_string(&configure_subpath)
        .map_err(|e| format!("Failed to encode subpath literal: {}", e))?;
    let root_literal = serde_json::to_string(package)
        .map_err(|e| format!("Failed to encode package literal: {}", e))?;

    // Existence check — exit 0 = configure() exported; exit 2 = no hook
    // (legitimate); exit 1 = real import error (syntax error, top-level
    // throw, missing dep). The Rust side surfaces stderr for case 1 instead
    // of swallowing it as "no hook".
    // Loader-agnostic "missing module" detection: Node's stock resolver sets
    // `e.code = ERR_MODULE_NOT_FOUND`, but `@swc-node/register` and other
    // ESM loaders wrap their own resolution failures without preserving the
    // code, so we also match by message. Anything else is treated as a real
    // import error (syntax error, runtime throw, etc.) and surfaced via
    // exit 1 rather than masked as "no hook".
    let check_script = format!(
        "const SUB = {sub}; \
         const ROOT = {root}; \
         function isMissingModule(e) {{ \
             if (!e) return false; \
             const code = e.code || ''; \
             if (code === 'ERR_MODULE_NOT_FOUND' || code === 'MODULE_NOT_FOUND' || code === 'ERR_PACKAGE_PATH_NOT_EXPORTED') return true; \
             const msg = e.message ? String(e.message) : String(e); \
             return /Cannot find (package|module)|cannot be resolved|ENOENT/i.test(msg); \
         }} \
         async function probe(spec) {{ \
             try {{ const m = await import(spec); return typeof m.configure === 'function'; }} \
             catch (e) {{ \
                 if (isMissingModule(e)) return false; \
                 console.error(e && e.stack ? e.stack : String(e)); \
                 process.exit(1); \
             }} \
         }} \
         if (await probe(SUB)) process.exit(0); \
         if (await probe(ROOT)) process.exit(0); \
         process.exit(2)",
        sub = sub_literal,
        root = root_literal,
    );
    // Capture stderr (instead of inherit) so we can post-classify the failure
    // mode. The two cases we want to disambiguate:
    //   - loader missing (`@swc-node/register` not in node_modules) → emit the
    //     actionable "pnpm add -D @swc-node/register" fix.
    //   - real import error inside the user's package (syntax, missing dep,
    //     top-level throw) → forward the original stderr verbatim so the
    //     user can debug their hook.
    //
    // Probe is bounded by `probe_timeout()` (default 10s) — module-level
    // deadlocks would otherwise hang `Command::status()` indefinitely.
    let mut child = std::process::Command::new("node")
        .args([
            "--import",
            "@swc-node/register/esm-register",
            "--input-type=module",
            "-e",
            &check_script,
        ])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to run configure check: {}", e))?;

    // Drain stderr on a thread BEFORE waiting: a probe that writes more than the
    // pipe buffer (~64KB) would block on a full pipe while the parent blocks in
    // wait_timeout — a deadlock surfaced as a misleading timeout (audit 2026-06-13).
    // On kill (timeout) the pipe closes and the reader returns, so the join is safe.
    let stderr_reader = child.stderr.take().map(|mut pipe| {
        std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    });

    let timeout = probe_timeout();
    let exit_status = match child.wait_timeout(timeout) {
        Ok(Some(status)) => status,
        Ok(None) => {
            // Timed out — kill, reap zombie, surface actionable error.
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "Configure existence-check timed out after {}s for '{}'.\n         \
                 This usually indicates a top-level `await` or module-level deadlock in the package.\n         \
                 Override the limit with REAM_PROBE_TIMEOUT_SECS=<n>.",
                timeout.as_secs(),
                package
            ));
        }
        Err(e) => return Err(format!("Failed to wait for configure check: {}", e)),
    };

    let stderr_bytes = stderr_reader
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();

    if exit_status.success() {
        // ok — fall through to configure run
    } else if exit_status.code() == Some(2) {
        return Ok(ConfigureOutcome::NoHook);
    } else {
        let stderr = String::from_utf8_lossy(&stderr_bytes);
        if is_loader_missing(&stderr) {
            return Err(format!(
                "@swc-node/register is required to load configure hooks.\n         \
                 Run: pnpm add -D @swc-node/register @swc/core\n         \
                 Then retry: ream configure {}",
                package
            ));
        }
        // Forward the original stderr so the user sees the real error
        // (syntax error, top-level throw, missing transitive dep, etc.).
        eprint!("{}", stderr);
        return Err(format!("Configure check failed for '{}'", package));
    }

    // Print the user-visible "Configuring..." line ONLY after the existence
    // check has confirmed the hook exists — under `ream configure` for a
    // hook-less package, the dispatcher would otherwise print "Configuring..."
    // then surface a "no hook" error, which is misleading.
    println!("\n  Configuring {}...\n", package);

    // The flags keep the order typed: `preserve_order` keeps it in the JSON,
    // and ECMAScript keeps it for non-integer keys.
    let script = format!(
        "{}{}",
        configure_input(package, &configure_subpath, force, flags)?,
        CONFIGURE_HOST
    );

    let status = std::process::Command::new("node")
        .args([
            "--import",
            "@swc-node/register/esm-register",
            "--input-type=module",
            "-e",
            &script,
        ])
        .status()
        .map_err(|e| format!("Failed to run configure: {}", e))?;

    if !status.success() {
        return Err(format!("Configure failed for '{}'", package));
    }

    println!();
    crate::ui::success(&format!("{} configured.", package));
    println!();
    Ok(ConfigureOutcome::Configured)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(list: &[(&str, &[&str])]) -> FlagPairs {
        list.iter()
            .map(|(key, values)| {
                (
                    (*key).to_string(),
                    values.iter().map(|v| Some((*v).to_string())).collect(),
                )
            })
            .collect()
    }

    /// The `INPUT` the host script receives, read back the way JS reads it.
    fn input(flags: &[(String, Vec<Option<String>>)], force: bool) -> Value {
        let line = configure_input("@scope/pkg", "@scope/pkg/configure", force, flags).unwrap();
        let literal = line
            .strip_prefix("const INPUT = JSON.parse(")
            .and_then(|rest| rest.strip_suffix(");\n"))
            .expect("one JSON.parse of one literal");
        let json: String = serde_json::from_str(literal).unwrap();
        serde_json::from_str(&json).unwrap()
    }

    fn raw(flags: &FlagPairs) -> Value {
        input(flags, false)["flags"].clone()
    }

    fn parsed(flags: &FlagPairs, force: bool) -> Value {
        input(flags, force)["parsedFlags"].clone()
    }

    #[test]
    fn empty_flags_produces_empty_object_literal() {
        assert_eq!(raw(&Vec::new()), serde_json::json!({}));
        assert_eq!(parsed(&Vec::new(), false), serde_json::json!({}));
    }

    #[test]
    fn single_flag_round_trips() {
        let flags = pairs(&[("transports", &["smtp"])]);
        assert_eq!(raw(&flags)["transports"], serde_json::json!(["smtp"]));
    }

    #[test]
    fn special_characters_survive_round_trip() {
        let flags = pairs(&[
            ("name", &["hello world"]),
            ("regex", &["a.*b"]),
            ("quote", &["he said \"hi\""]),
            ("backslash", &["a\\b"]),
            ("newline", &["a\nb"]),
            ("unicode", &["héllo"]),
            ("breakout", &["\"); process.exit(9); (\""]),
        ]);
        let parsed = raw(&flags);
        assert_eq!(
            parsed["breakout"],
            serde_json::json!(["\"); process.exit(9); (\""])
        );
        assert_eq!(parsed["name"], serde_json::json!(["hello world"]));
        assert_eq!(parsed["regex"], serde_json::json!(["a.*b"]));
        assert_eq!(parsed["quote"], serde_json::json!(["he said \"hi\""]));
        assert_eq!(parsed["backslash"], serde_json::json!(["a\\b"]));
        assert_eq!(parsed["newline"], serde_json::json!(["a\nb"]));
        assert_eq!(parsed["unicode"], serde_json::json!(["héllo"]));
    }

    #[test]
    fn multi_value_flag_preserves_order() {
        let flags = pairs(&[("transports", &["smtp", "resend", "ses"])]);
        let parsed = raw(&flags);
        assert_eq!(
            parsed["transports"],
            serde_json::json!(["smtp", "resend", "ses"])
        );
    }

    #[test]
    fn key_insertion_order_is_preserved() {
        // serde_json with `preserve_order` keeps insertion order — important
        // because the user's typed flag order is a meaningful contract for
        // configure() hooks that iterate Object.keys(flags).
        let flags = pairs(&[("zeta", &["1"]), ("alpha", &["2"]), ("middle", &["3"])]);
        let parsed = raw(&flags);
        // Iterate the JSON object — `preserve_order` makes this insertion order.
        let keys: Vec<&str> = parsed
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, vec!["zeta", "alpha", "middle"]);
    }

    #[test]
    fn a_bare_flag_is_the_string_true_in_the_raw_record() {
        let flags: FlagPairs = vec![("ssl".to_string(), vec![None])];
        assert_eq!(raw(&flags), serde_json::json!({ "ssl": ["true"] }));
    }

    #[test]
    fn parsed_flags_read_the_way_the_console_parses_them() {
        // `--store=redis --retries=3 --ratio=0.5 --tag a --tag b --ssl --no-color`
        let mut flags = pairs(&[
            ("store", &["redis"]),
            ("retries", &["3"]),
            ("ratio", &["0.5"]),
            ("tag", &["a", "b"]),
        ]);
        flags.push(("ssl".to_string(), vec![None]));
        flags.push(("no-color".to_string(), vec![None]));
        assert_eq!(
            parsed(&flags, true),
            serde_json::json!({
                "store": "redis",
                "retries": 3,
                "ratio": 0.5,
                "tag": ["a", "b"],
                "ssl": true,
                "color": false,
                "force": true,
            })
        );
    }

    #[test]
    fn force_is_absent_from_parsed_flags_unless_passed() {
        assert_eq!(
            parsed(&pairs(&[("a", &["b"])]), false),
            serde_json::json!({ "a": "b" })
        );
    }

    #[test]
    fn a_typed_true_stays_a_string_and_a_negation_with_a_value_is_kept() {
        // `--x=true` is the string the user typed; only a bare flag is `true`.
        // `--no-x=y` names a flag called `no-x`.
        let flags = pairs(&[("x", &["true"]), ("no-y", &["z"])]);
        assert_eq!(
            parsed(&flags, false),
            serde_json::json!({ "x": "true", "no-y": "z" })
        );
    }

    #[test]
    fn only_yargs_numbers_become_numbers() {
        for (text, expected) in [
            ("42", serde_json::json!(42)),
            ("-7", serde_json::json!(-7)),
            ("1e3", serde_json::json!(1000)),
            (".5", serde_json::json!(0.5)),
            ("0x1f", serde_json::json!(31)),
            ("0.25", serde_json::json!(0.25)),
            // A leading zero is an identifier, not octal: kept as typed.
            ("0123", serde_json::json!("0123")),
            ("1.2.3", serde_json::json!("1.2.3")),
            ("12abc", serde_json::json!("12abc")),
            ("Infinity", serde_json::json!("Infinity")),
            ("NaN", serde_json::json!("NaN")),
            ("", serde_json::json!("")),
            ("-", serde_json::json!("-")),
            // Past 2^53 the number would not be what was typed.
            (
                "12345678901234567890",
                serde_json::json!("12345678901234567890"),
            ),
        ] {
            assert_eq!(flag_value(&Some(text.to_string())), expected, "{text}");
        }
    }

    #[test]
    fn is_valid_npm_name_accepts_scoped() {
        assert!(is_valid_npm_name("@c9up/atlas"));
    }

    #[test]
    fn is_valid_npm_name_rejects_dotdot_substring() {
        assert!(!is_valid_npm_name("../evil"));
        assert!(!is_valid_npm_name("@c9up/..evil"));
    }

    #[test]
    fn is_valid_npm_name_rejects_leading_dot_in_tail() {
        assert!(!is_valid_npm_name("@x/.npmrc"));
        assert!(!is_valid_npm_name("@x/.bin"));
        assert!(!is_valid_npm_name(".hidden"));
    }

    #[test]
    fn is_valid_npm_name_rejects_leading_underscore_in_tail() {
        assert!(!is_valid_npm_name("@x/_internal"));
        assert!(!is_valid_npm_name("_private"));
    }

    #[test]
    fn is_valid_npm_name_rejects_empty_scope_or_tail() {
        assert!(!is_valid_npm_name("@/atlas"));
        assert!(!is_valid_npm_name("@c9up/"));
        assert!(!is_valid_npm_name(""));
    }

    #[test]
    fn is_loader_missing_detects_node_stock_resolver() {
        let stderr = "node:internal/modules/run_main:122\n\
                      Error [ERR_MODULE_NOT_FOUND]: Cannot find package '@swc-node/register' \
                      imported from /tmp/proj/\n";
        assert!(is_loader_missing(stderr));
    }

    #[test]
    fn is_loader_missing_detects_module_not_found_variant() {
        let stderr = "Error: Cannot find module '@swc-node/register/esm-register'\n";
        assert!(is_loader_missing(stderr));
    }

    #[test]
    fn is_loader_missing_rejects_unrelated_module_not_found() {
        // The user's package has a missing dep — NOT a loader-missing case.
        let stderr = "Error [ERR_MODULE_NOT_FOUND]: Cannot find package '@user/missing-dep' \
                      imported from /tmp/proj/node_modules/@community/postmark/src/index.ts\n";
        assert!(!is_loader_missing(stderr));
    }

    #[test]
    fn is_loader_missing_rejects_user_package_syntax_error() {
        let stderr = "/tmp/proj/node_modules/@community/postmark/src/configure.ts:5\n\
                      SyntaxError: Unexpected token '}'\n";
        assert!(!is_loader_missing(stderr));
    }

    #[test]
    fn parse_probe_timeout_uses_default_when_unset() {
        assert_eq!(parse_probe_timeout(None), Duration::from_secs(10));
    }

    #[test]
    fn parse_probe_timeout_accepts_positive_integer() {
        assert_eq!(parse_probe_timeout(Some("30")), Duration::from_secs(30));
        assert_eq!(parse_probe_timeout(Some("1")), Duration::from_secs(1));
        assert_eq!(parse_probe_timeout(Some("3600")), Duration::from_secs(3600));
    }

    #[test]
    fn parse_probe_timeout_falls_back_to_default_on_zero_negative_or_garbage() {
        // 0 disabled → defensive default (a 0s timeout would kill every probe).
        assert_eq!(parse_probe_timeout(Some("0")), Duration::from_secs(10));
        assert_eq!(parse_probe_timeout(Some("-5")), Duration::from_secs(10));
        assert_eq!(parse_probe_timeout(Some("abc")), Duration::from_secs(10));
        assert_eq!(parse_probe_timeout(Some("")), Duration::from_secs(10));
        assert_eq!(parse_probe_timeout(Some("10.5")), Duration::from_secs(10));
    }
}
