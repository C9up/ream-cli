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

/// Configure a package with optional flags forwarded to its `configure()`
/// hook. Flags are encoded as `Record<string, string[]>` and reach the hook
/// as the second argument (`configure(codemods, flags)`).
///
/// Returns `Ok(NoHook)` instead of erroring when the package exists but does
/// not export a configure() function — callers (notably `ream add`) decide
/// whether that case is fatal.
pub fn configure_with_flags(
    package: &str,
    force: bool,
    flags: &[(String, Vec<String>)],
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

    // Encode flags as `Record<string, string[]>` preserving the user's
    // insertion order. ECMAScript guarantees iteration order for non-integer
    // string keys, so the order the user typed reaches the hook unchanged.
    let mut obj = Map::new();
    for (k, v) in flags {
        let arr: Vec<Value> = v.iter().map(|s| Value::String(s.clone())).collect();
        obj.insert(k.clone(), Value::Array(arr));
    }
    let flags_json = serde_json::to_string(&Value::Object(obj))
        .map_err(|e| format!("Failed to encode flags: {}", e))?;
    // Double-encode so the inline script body contains a valid JS string
    // literal that `JSON.parse` then turns back into the object — sidesteps
    // hand-escaping `'`, `"`, `\` in flag values.
    let flags_literal = serde_json::to_string(&flags_json)
        .map_err(|e| format!("Failed to encode flags literal: {}", e))?;

    let force_str = if force { "true" } else { "false" };
    let script = format!(
        "import {{ createCodemods }} from '@c9up/ream'; \
         const FLAGS = JSON.parse({flags_literal}); \
         const SUB = {sub}; \
         const ROOT = {root}; \
         let configure; \
         try {{ const m = await import(SUB); configure = m.configure; }} catch {{}} \
         if (!configure) {{ const m = await import(ROOT); configure = m.configure; }} \
         await configure(createCodemods({{ force: {force} }}), FLAGS);",
        flags_literal = flags_literal,
        sub = sub_literal,
        root = root_literal,
        force = force_str,
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

    println!("\n  \x1b[32mDone!\x1b[0m {} configured.\n", package);
    Ok(ConfigureOutcome::Configured)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirror the production flag-encoding path. Returns an inline Node
    /// script fragment of the shape `const FLAGS = JSON.parse(<literal>); `
    /// where `<literal>` is the JS string literal containing the encoded
    /// `Record<string, string[]>` (the same payload `configure_with_flags`
    /// embeds into the inline `-e <script>`).
    fn build_script(flags: &[(String, Vec<String>)]) -> String {
        let mut obj = Map::new();
        for (k, v) in flags {
            let arr: Vec<Value> = v.iter().map(|s| Value::String(s.clone())).collect();
            obj.insert(k.clone(), Value::Array(arr));
        }
        let flags_json = serde_json::to_string(&Value::Object(obj)).unwrap();
        let flags_literal = serde_json::to_string(&flags_json).unwrap();
        format!("const FLAGS = JSON.parse({}); ", flags_literal)
    }

    fn extract_parsed(script: &str) -> Value {
        let inner_start = script.find("JSON.parse(").unwrap() + "JSON.parse(".len();
        let inner_end = script.rfind(')').unwrap();
        let literal = &script[inner_start..inner_end];
        let inner_json: String = serde_json::from_str(literal).unwrap();
        serde_json::from_str(&inner_json).unwrap()
    }

    #[test]
    fn empty_flags_produces_empty_object_literal() {
        let s = build_script(&[]);
        assert!(s.contains("JSON.parse(\"{}\")"), "got: {}", s);
    }

    #[test]
    fn single_flag_round_trips() {
        let flags = vec![("transports".to_string(), vec!["smtp".to_string()])];
        let parsed = extract_parsed(&build_script(&flags));
        assert_eq!(parsed["transports"], serde_json::json!(["smtp"]));
    }

    #[test]
    fn special_characters_survive_round_trip() {
        let flags = vec![
            ("name".to_string(), vec!["hello world".to_string()]),
            ("regex".to_string(), vec!["a.*b".to_string()]),
            ("quote".to_string(), vec!["he said \"hi\"".to_string()]),
            ("backslash".to_string(), vec!["a\\b".to_string()]),
            ("newline".to_string(), vec!["a\nb".to_string()]),
            ("unicode".to_string(), vec!["héllo".to_string()]),
        ];
        let parsed = extract_parsed(&build_script(&flags));
        assert_eq!(parsed["name"], serde_json::json!(["hello world"]));
        assert_eq!(parsed["regex"], serde_json::json!(["a.*b"]));
        assert_eq!(parsed["quote"], serde_json::json!(["he said \"hi\""]));
        assert_eq!(parsed["backslash"], serde_json::json!(["a\\b"]));
        assert_eq!(parsed["newline"], serde_json::json!(["a\nb"]));
        assert_eq!(parsed["unicode"], serde_json::json!(["héllo"]));
    }

    #[test]
    fn multi_value_flag_preserves_order() {
        let flags = vec![(
            "transports".to_string(),
            vec!["smtp".to_string(), "resend".to_string(), "ses".to_string()],
        )];
        let parsed = extract_parsed(&build_script(&flags));
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
        let flags = vec![
            ("zeta".to_string(), vec!["1".to_string()]),
            ("alpha".to_string(), vec!["2".to_string()]),
            ("middle".to_string(), vec!["3".to_string()]),
        ];
        let parsed = extract_parsed(&build_script(&flags));
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
