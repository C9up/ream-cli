//! `ream add <pkg>` — install + configure in one step.
//!
//! Auto-detects the project's package manager from its lockfile (pnpm > yarn >
//! npm precedence; bun deferred), runs `<pm> add [-D] <pkg>` with stdio
//! inherited, then dispatches to the existing configure() hook with any
//! trailing flags forwarded as `Record<string, string[]>`.
//!
//! TEST HOOK (do NOT document publicly): set `REAM_ADD_DRY_RUN=1` together
//! with `REAM_ADD_DRY_RUN_LOG=<path>` to capture the would-be PM command line
//! into <path> instead of executing it. Used by `tests/add_test.rs` to keep
//! integration tests hermetic without touching the npm registry.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::codemods;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackageManager {
    Pnpm,
    Npm,
    Yarn,
}

impl PackageManager {
    fn binary(&self) -> &'static str {
        match self {
            Self::Pnpm => "pnpm",
            Self::Npm => "npm",
            Self::Yarn => "yarn",
        }
    }
}

fn lockfile_for(pm: PackageManager) -> &'static str {
    match pm {
        PackageManager::Pnpm => "pnpm-lock.yaml",
        PackageManager::Yarn => "yarn.lock",
        PackageManager::Npm => "package-lock.json",
    }
}

/// Detect the package manager by walking up from cwd looking for a lockfile.
///
/// Precedence within a single directory: pnpm > yarn > npm (first match wins).
/// The walk stops at:
///   - the first directory containing any lockfile (return it);
///   - a `.git` boundary (treat as repo root, stop without ascending further);
///   - the filesystem root.
///
/// Fallbacks when no lockfile is found anywhere:
///   - any ancestor with `pnpm-workspace.yaml` → treat as pnpm (workspace
///     marker hints the PM even before the first install creates a lockfile);
///   - any ancestor with `bun.lockb` / `bun.lock` → emit a Bun-specific error
///     (bun is intentionally deferred — see story 50.1 scope cuts);
///   - otherwise the generic "couldn't detect" error.
///
/// Returns the detected PM plus the list of secondary lockfiles in the
/// dispatch directory (for the "ignored lockfile" warning at the call site).
fn detect_package_manager(package: &str) -> Result<(PackageManager, Vec<&'static str>), String> {
    let cwd = std::env::current_dir()
        .map_err(|e| format!("Failed to read current directory: {}", e))?;
    detect_package_manager_from(&cwd, package)
}

fn detect_package_manager_from(
    start: &Path,
    package: &str,
) -> Result<(PackageManager, Vec<&'static str>), String> {
    let candidates: [(PackageManager, &'static str); 3] = [
        (PackageManager::Pnpm, "pnpm-lock.yaml"),
        (PackageManager::Yarn, "yarn.lock"),
        (PackageManager::Npm, "package-lock.json"),
    ];

    let mut current: Option<PathBuf> = Some(start.to_path_buf());
    let mut workspace_hint = false;
    let mut bun_present = false;

    while let Some(dir) = current {
        let mut found_in_dir: Vec<(PackageManager, &'static str)> = Vec::new();
        for &(pm, file) in &candidates {
            if dir.join(file).exists() {
                found_in_dir.push((pm, file));
            }
        }
        if !found_in_dir.is_empty() {
            let (pm, _) = found_in_dir[0];
            let secondary: Vec<&'static str> =
                found_in_dir[1..].iter().map(|(_, f)| *f).collect();
            return Ok((pm, secondary));
        }
        if !workspace_hint && dir.join("pnpm-workspace.yaml").exists() {
            workspace_hint = true;
        }
        if !bun_present
            && (dir.join("bun.lockb").exists() || dir.join("bun.lock").exists())
        {
            bun_present = true;
        }
        // Repo boundary — stop walking once we hit `.git`.
        if dir.join(".git").exists() {
            break;
        }
        current = dir.parent().map(Path::to_path_buf);
    }

    if workspace_hint {
        return Ok((PackageManager::Pnpm, Vec::new()));
    }

    if bun_present {
        return Err(format!(
            "Bun is not yet supported by `ream add`.\n  \
             Run `bun add {0}` then `ream configure {0}` instead.",
            package
        ));
    }

    Err(format!(
        "Couldn't detect package manager (no pnpm-lock.yaml / yarn.lock / package-lock.json found).\n  \
         Run 'pnpm add {} && ream configure {}' yourself, or initialise a lockfile first.",
        package, package
    ))
}

fn install_args(pm: PackageManager, dev: bool, package: &str) -> Vec<String> {
    match pm {
        PackageManager::Pnpm | PackageManager::Yarn => {
            let mut a = vec!["add".to_string()];
            if dev {
                a.push("-D".to_string());
            }
            a.push(package.to_string());
            a
        }
        PackageManager::Npm => {
            let mut a = vec!["install".to_string()];
            if dev {
                a.push("--save-dev".to_string());
            }
            a.push(package.to_string());
            a
        }
    }
}

/// Parse an npm-spec string into `(bare_name, full_spec)`.
///
/// Accepts:
///   - `pkg`                       → bare = `pkg`
///   - `pkg@1.2.3`                 → bare = `pkg`
///   - `pkg@^1.2.3`                → bare = `pkg`
///   - `pkg@latest`                → bare = `pkg` (dist-tag)
///   - `@scope/pkg`                → bare = `@scope/pkg`
///   - `@scope/pkg@1.2.3`          → bare = `@scope/pkg`
///   - `pkg@npm:other-pkg`         → bare = `pkg` (npm alias; PM installs `other-pkg`
///                                   under `node_modules/pkg`, so `pkg` is what we
///                                   import — see https://docs.npmjs.com/cli/v10/configuring-npm/package-json#dependencies).
///
/// Rejects (with actionable two-step error):
///   - URLs / tarballs (`https://…tgz`, `http://`)         — registry-of-record unknown
///   - git refs (`git+https://`, `git+ssh://`, `github:`,  — bare name not derivable
///     `git:`)
///   - file paths (`file:./…`, `./…`, `../…`, `/abs/…`)    — local install requires
///                                                            reading the target's
///                                                            package.json
///
/// `bare_name` is what reaches `configure_with_flags` (the dynamic `import()` spec
/// MUST be the bare package name; embedding a version specifier would break Node
/// module resolution). `full_spec` is forwarded verbatim to the package manager so
/// version pins, dist-tags, and `npm:` aliases all work.
///
/// `pub(crate)` so the `tests` module can exercise it directly.
pub(crate) fn parse_npm_spec(input: &str) -> Result<(String, String), String> {
    if input.is_empty() {
        return Err("Empty package spec".to_string());
    }

    // Order matters: `git+https://…` matches both git-ref and URL patterns,
    // but the more specific git-ref reason is the actionable one for the user
    // (the workaround they want is `pnpm add git+…` not "drop the protocol").
    let forbidden_kind = if input.starts_with("git+")
        || input.starts_with("github:")
        || input.starts_with("git:")
    {
        Some("git ref")
    } else if input.contains("://") {
        Some("URL/tarball")
    } else if input.starts_with("file:")
        || input.starts_with("./")
        || input.starts_with("../")
        || input.starts_with('/')
    {
        Some("file path")
    } else {
        None
    };
    if let Some(kind) = forbidden_kind {
        return Err(format!(
            "Unsupported package spec ({}): {}\n         \
             Use the manual two-step: pnpm add {} && ream configure <bare-name>",
            kind, input, input
        ));
    }

    let bare = if let Some(rest) = input.strip_prefix('@') {
        // Scoped: split on first `/` (the scope/package separator), then on the
        // first `@` inside the post-`/` segment (the version-spec separator).
        let slash = rest
            .find('/')
            .ok_or_else(|| format!("Invalid scoped package spec (missing '/'): {}", input))?;
        let scope_end = slash + 1; // index in `input` of the char after `@scope/`
        match input[scope_end + 1..].find('@') {
            Some(at) => input[..scope_end + 1 + at].to_string(),
            None => input.to_string(),
        }
    } else {
        // Unscoped: split on first `@` (cannot be position 0 — that's a scope).
        match input.find('@') {
            Some(at) => input[..at].to_string(),
            None => input.to_string(),
        }
    };

    if !codemods::is_valid_npm_name(&bare) {
        return Err(format!("Invalid package name: {}", bare));
    }

    Ok((bare, input.to_string()))
}

/// Parse the trailing `Vec<String>` clap captured into a flags map.
///
/// - `--key=value`               → `[("key", ["value"])]`
/// - `--key value` (no `=`)      → `[("key", ["value"])]`
/// - `--key` (boolean)           → `[("key", ["true"])]`
/// - repeated keys accumulate    → `[("key", ["a", "b"])]`
/// - bare positional arg         → `Err`
///
/// Made `pub` so the `Configure` subcommand dispatcher in `main.rs` can reuse
/// it. `--dev` and `--force` are eaten by clap before they reach this parser.
pub fn parse_flag_pairs(raw: &[String]) -> Result<Vec<(String, Vec<String>)>, String> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let item = &raw[i];
        if !item.starts_with("--") {
            return Err(format!(
                "Unexpected positional argument: {}. Use --key=value for package flags.",
                item
            ));
        }
        let stripped = &item[2..];
        let (key, value) = if let Some(eq) = stripped.find('=') {
            let (k, v) = stripped.split_at(eq);
            let v_str = v[1..].to_string();
            if v_str.is_empty() {
                return Err(format!(
                    "Empty flag value in '{}' — use --{}=<value> or just --{} for boolean.",
                    item, k, k
                ));
            }
            (k.to_string(), Some(v_str))
        } else {
            // Lookahead: consume next item as value iff it does NOT start with `--`.
            let next = raw.get(i + 1);
            match next {
                Some(n) if !n.starts_with("--") => {
                    i += 1;
                    (stripped.to_string(), Some(n.clone()))
                }
                _ => (stripped.to_string(), None),
            }
        };

        if key.is_empty() {
            return Err(format!("Empty flag name in: {}", item));
        }
        // Validate flag-key shape — must start with an ASCII letter, then
        // [A-Za-z0-9_-]. Stops whitespace/control-char/Unicode keys from
        // sneaking into the JSON object that reaches the configure() hook.
        let mut chars = key.chars();
        let first = chars.next().unwrap();
        if !first.is_ascii_alphabetic() {
            return Err(format!(
                "Invalid flag name '--{}': must start with a letter (a-z, A-Z).",
                key
            ));
        }
        if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            return Err(format!(
                "Invalid flag name '--{}': only letters, digits, '_' and '-' are allowed.",
                key
            ));
        }
        // `--dev` / `--force` should never reach this parser (clap consumes
        // them as named subcommand flags). Guard defensively in case of a
        // clap reshuffle.
        if key == "dev" || key == "force" {
            return Err(format!(
                "Reserved flag --{} cannot be forwarded to configure() (it is consumed by `ream add`/`ream configure` itself).",
                key
            ));
        }

        let v = value.unwrap_or_else(|| "true".to_string());
        if let Some(slot) = out.iter_mut().find(|(k, _)| k == &key) {
            slot.1.push(v);
        } else {
            out.push((key, vec![v]));
        }
        i += 1;
    }
    Ok(out)
}

pub fn run(spec: &str, dev: bool, force: bool, flags: &[String]) -> Result<(), String> {
    // Pre-flight: in a Ream project.
    if !Path::new("package.json").exists() {
        return Err("Not in a Ream project (no package.json found)".to_string());
    }
    // Split the user input into bare name (used for the dynamic `import()`
    // dispatched to `configure_with_flags`) and the full spec (forwarded
    // verbatim to the package manager so `react@19`, `@c9up/atlas@^1.2.3`,
    // dist-tags, and `npm:` aliases all work).
    let (bare, full_spec) = parse_npm_spec(spec)?;

    // Parse flags BEFORE install — fail fast on malformed input rather than
    // half-way through the operation.
    let pairs = parse_flag_pairs(flags)?;

    let (pm, secondary) = detect_package_manager(&bare)?;
    for ignored in &secondary {
        eprintln!(
            "  warning: ignoring secondary lockfile {} (using {} per detection precedence)",
            ignored,
            lockfile_for(pm)
        );
    }

    let args = install_args(pm, dev, &full_spec);

    println!("\n  Adding {} with {}...\n", full_spec, pm.binary());

    let dry_run = std::env::var("REAM_ADD_DRY_RUN").ok().as_deref() == Some("1");
    if dry_run {
        // Capture the would-be command line for the integration tests instead
        // of spawning a real PM. The capture log is appended to with a single
        // line of `<pm> arg1 arg2 ...` so the test can read it back.
        //
        // REAM_ADD_DRY_RUN_LOG is REQUIRED when DRY_RUN=1 — without it, a stray
        // `REAM_ADD_DRY_RUN=1` exported in a developer's shell would silently
        // skip every install and `ream add` would print "Done!" without ever
        // calling the PM. Surface a hard error instead, naming the env vars so
        // the user can unset them.
        let log_path = std::env::var("REAM_ADD_DRY_RUN_LOG").map_err(|_| {
            "REAM_ADD_DRY_RUN=1 requires REAM_ADD_DRY_RUN_LOG=<path> (test-only \
             hook). Unset both to install for real."
                .to_string()
        })?;
        use std::io::Write;
        let line = format!("{} {}\n", pm.binary(), args.join(" "));
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| format!("Failed to open dry-run log: {}", e))?;
        f.write_all(line.as_bytes())
            .map_err(|e| format!("Failed to write dry-run log: {}", e))?;
    } else {
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let status = Command::new(pm.binary())
            .args(&arg_refs)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|e| format!("Failed to run '{}': {}", pm.binary(), e))?;
        if !status.success() {
            return Err(format!(
                "ream add: install failed ({} exited with code {})",
                pm.binary(),
                status.code().unwrap_or(-1)
            ));
        }
    }

    // Dispatch to configure with the parsed flags. A package without a
    // configure() hook is downgraded to a non-error info line under
    // `ream add` (the install succeeded; the absence of a hook is a property
    // of the package, not a runtime failure).
    //
    // Configure dispatch uses the BARE name (not the full spec): the dynamic
    // `import()` inside `configure_with_flags` MUST resolve a package name —
    // Node's resolver does not understand version specifiers in module
    // identifiers (`import 'react@19'` does not work). The version reached
    // disk via the package manager above; from here on `react` resolves to
    // whatever pnpm/yarn/npm just installed.
    match codemods::configure_with_flags(&bare, force, &pairs)? {
        codemods::ConfigureOutcome::Configured => {
            println!("\n  \x1b[32mDone!\x1b[0m {} added.\n", full_spec);
        }
        codemods::ConfigureOutcome::NoHook => {
            println!(
                "\n  Note: {} has no configure() hook — package installed; see the package's README for any manual setup.\n",
                full_spec
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_equals_flag() {
        let raw = vec!["--transports=smtp".to_string()];
        let out = parse_flag_pairs(&raw).unwrap();
        assert_eq!(out, vec![("transports".to_string(), vec!["smtp".to_string()])]);
    }

    #[test]
    fn parse_repeated_key_accumulates_in_order() {
        let raw = vec![
            "--transports=smtp".to_string(),
            "--transports=resend".to_string(),
        ];
        let out = parse_flag_pairs(&raw).unwrap();
        assert_eq!(
            out,
            vec![(
                "transports".to_string(),
                vec!["smtp".to_string(), "resend".to_string()]
            )]
        );
    }

    #[test]
    fn parse_boolean_flag_no_value() {
        let raw = vec!["--ssl".to_string()];
        let out = parse_flag_pairs(&raw).unwrap();
        assert_eq!(out, vec![("ssl".to_string(), vec!["true".to_string()])]);
    }

    #[test]
    fn parse_value_no_equals_consumes_next_item() {
        let raw = vec!["--driver".to_string(), "postgres".to_string()];
        let out = parse_flag_pairs(&raw).unwrap();
        assert_eq!(
            out,
            vec![("driver".to_string(), vec!["postgres".to_string()])]
        );
    }

    #[test]
    fn parse_two_boolean_flags_in_a_row() {
        // `--ssl --verbose` — both treated as boolean since neither has `=`
        // and the lookahead sees another `--` flag.
        let raw = vec!["--ssl".to_string(), "--verbose".to_string()];
        let out = parse_flag_pairs(&raw).unwrap();
        assert_eq!(
            out,
            vec![
                ("ssl".to_string(), vec!["true".to_string()]),
                ("verbose".to_string(), vec!["true".to_string()]),
            ]
        );
    }

    #[test]
    fn parse_rejects_positional_argument() {
        let raw = vec!["positional".to_string()];
        let err = parse_flag_pairs(&raw).unwrap_err();
        assert!(err.contains("Unexpected positional argument"), "got: {}", err);
    }

    #[test]
    fn parse_rejects_reserved_dev_flag() {
        let raw = vec!["--dev=true".to_string()];
        let err = parse_flag_pairs(&raw).unwrap_err();
        assert!(err.contains("Reserved flag --dev"), "got: {}", err);
    }

    #[test]
    fn parse_rejects_reserved_force_flag() {
        let raw = vec!["--force=yes".to_string()];
        let err = parse_flag_pairs(&raw).unwrap_err();
        assert!(err.contains("Reserved flag --force"), "got: {}", err);
    }

    #[test]
    fn parse_value_with_equals_inside() {
        // `--regex=a=b` — split on FIRST `=` only.
        let raw = vec!["--regex=a=b".to_string()];
        let out = parse_flag_pairs(&raw).unwrap();
        assert_eq!(out, vec![("regex".to_string(), vec!["a=b".to_string()])]);
    }

    #[test]
    fn parse_rejects_empty_equals_value() {
        // `--key=` — empty value is ambiguous (boolean? typo?) → reject.
        let raw = vec!["--password=".to_string()];
        let err = parse_flag_pairs(&raw).unwrap_err();
        assert!(err.contains("Empty flag value"), "got: {}", err);
    }

    #[test]
    fn parse_rejects_key_starting_with_digit() {
        let raw = vec!["--1stuff=value".to_string()];
        let err = parse_flag_pairs(&raw).unwrap_err();
        assert!(
            err.contains("must start with a letter"),
            "got: {}",
            err
        );
    }

    #[test]
    fn parse_rejects_key_with_invalid_chars() {
        let raw = vec!["--bad key=value".to_string()];
        let err = parse_flag_pairs(&raw).unwrap_err();
        assert!(
            err.contains("only letters, digits"),
            "got: {}",
            err
        );
    }

    #[test]
    fn parse_accepts_kebab_and_snake_keys() {
        let raw = vec![
            "--http-only=true".to_string(),
            "--user_id=42".to_string(),
        ];
        let out = parse_flag_pairs(&raw).unwrap();
        assert_eq!(
            out,
            vec![
                ("http-only".to_string(), vec!["true".to_string()]),
                ("user_id".to_string(), vec!["42".to_string()]),
            ]
        );
    }

    #[test]
    fn install_args_pnpm_regular() {
        assert_eq!(
            install_args(PackageManager::Pnpm, false, "@c9up/atlas"),
            vec!["add".to_string(), "@c9up/atlas".to_string()]
        );
    }

    #[test]
    fn install_args_pnpm_dev() {
        assert_eq!(
            install_args(PackageManager::Pnpm, true, "@c9up/atlas"),
            vec!["add".to_string(), "-D".to_string(), "@c9up/atlas".to_string()]
        );
    }

    #[test]
    fn install_args_yarn_regular() {
        assert_eq!(
            install_args(PackageManager::Yarn, false, "@c9up/photon"),
            vec!["add".to_string(), "@c9up/photon".to_string()]
        );
    }

    #[test]
    fn install_args_yarn_dev() {
        assert_eq!(
            install_args(PackageManager::Yarn, true, "@c9up/photon"),
            vec!["add".to_string(), "-D".to_string(), "@c9up/photon".to_string()]
        );
    }

    #[test]
    fn install_args_npm_regular() {
        assert_eq!(
            install_args(PackageManager::Npm, false, "@c9up/warden"),
            vec!["install".to_string(), "@c9up/warden".to_string()]
        );
    }

    #[test]
    fn install_args_npm_dev() {
        assert_eq!(
            install_args(PackageManager::Npm, true, "@c9up/warden"),
            vec!["install".to_string(), "--save-dev".to_string(), "@c9up/warden".to_string()]
        );
    }

    #[test]
    fn detect_pm_walks_up_to_find_lockfile() {
        // Synthesize a parent → child layout in /tmp and confirm the walk-up
        // finds the lockfile in the parent. .git on the parent acts as the
        // boundary so the walk doesn't escape into the host filesystem.
        let parent = std::env::temp_dir().join(format!(
            "ream-detect-walkup-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let child = parent.join("apps").join("web");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(parent.join("pnpm-lock.yaml"), "").unwrap();
        std::fs::create_dir_all(parent.join(".git")).unwrap();

        let (pm, secondary) =
            detect_package_manager_from(&child, "@c9up/atlas").unwrap();
        assert_eq!(pm, PackageManager::Pnpm);
        assert!(secondary.is_empty());

        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn detect_pm_workspace_yaml_hints_pnpm_without_lockfile() {
        let dir = std::env::temp_dir().join(format!(
            "ream-detect-ws-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pnpm-workspace.yaml"), "").unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();

        let (pm, secondary) =
            detect_package_manager_from(&dir, "@c9up/atlas").unwrap();
        assert_eq!(pm, PackageManager::Pnpm);
        assert!(secondary.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_pm_bun_lockfile_emits_specific_error() {
        let dir = std::env::temp_dir().join(format!(
            "ream-detect-bun-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bun.lockb"), "").unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();

        let err = detect_package_manager_from(&dir, "@c9up/atlas").unwrap_err();
        assert!(
            err.contains("Bun is not yet supported"),
            "expected Bun-specific error, got: {}",
            err
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_npm_spec_bare_unscoped() {
        let (bare, full) = parse_npm_spec("react").unwrap();
        assert_eq!(bare, "react");
        assert_eq!(full, "react");
    }

    #[test]
    fn parse_npm_spec_bare_scoped() {
        let (bare, full) = parse_npm_spec("@c9up/atlas").unwrap();
        assert_eq!(bare, "@c9up/atlas");
        assert_eq!(full, "@c9up/atlas");
    }

    #[test]
    fn parse_npm_spec_unscoped_version_pin() {
        let (bare, full) = parse_npm_spec("react@19").unwrap();
        assert_eq!(bare, "react");
        assert_eq!(full, "react@19");
    }

    #[test]
    fn parse_npm_spec_scoped_version_pin() {
        let (bare, full) = parse_npm_spec("@c9up/atlas@^1.2.3").unwrap();
        assert_eq!(bare, "@c9up/atlas");
        assert_eq!(full, "@c9up/atlas@^1.2.3");
    }

    #[test]
    fn parse_npm_spec_dist_tag() {
        let (bare, full) = parse_npm_spec("react@latest").unwrap();
        assert_eq!(bare, "react");
        assert_eq!(full, "react@latest");
    }

    #[test]
    fn parse_npm_spec_npm_alias() {
        // `pnpm add postmark@npm:other-pkg` installs `other-pkg` under
        // `node_modules/postmark` — the alias name `postmark` is what `import`
        // resolves, so it's the right `bare_name` for configure dispatch.
        let (bare, full) = parse_npm_spec("postmark@npm:@upstream/postmark").unwrap();
        assert_eq!(bare, "postmark");
        assert_eq!(full, "postmark@npm:@upstream/postmark");
    }

    #[test]
    fn parse_npm_spec_rejects_https_tarball() {
        let err = parse_npm_spec("https://registry.npmjs.org/react/-/react-19.0.0.tgz")
            .unwrap_err();
        assert!(err.contains("URL/tarball"), "got: {}", err);
        assert!(err.contains("manual two-step"), "got: {}", err);
    }

    #[test]
    fn parse_npm_spec_rejects_git_ref() {
        let err = parse_npm_spec("git+https://github.com/foo/bar.git").unwrap_err();
        assert!(err.contains("git ref"), "got: {}", err);

        let err2 = parse_npm_spec("github:foo/bar").unwrap_err();
        assert!(err2.contains("git ref"), "got: {}", err2);
    }

    #[test]
    fn parse_npm_spec_rejects_file_path() {
        let err = parse_npm_spec("file:./local-pkg").unwrap_err();
        assert!(err.contains("file path"), "got: {}", err);

        let err2 = parse_npm_spec("./local-pkg").unwrap_err();
        assert!(err2.contains("file path"), "got: {}", err2);

        let err3 = parse_npm_spec("/abs/path").unwrap_err();
        assert!(err3.contains("file path"), "got: {}", err3);
    }

    #[test]
    fn parse_npm_spec_rejects_invalid_bare_name() {
        // `..` in the bare-name path component must still be caught — would be
        // a path-traversal attempt against the dynamic `import()`.
        let err = parse_npm_spec("../evil@1.0.0").unwrap_err();
        assert!(err.contains("file path"), "../ is rejected as file path first; got: {}", err);

        let err2 = parse_npm_spec("@x/.bin@1.0.0").unwrap_err();
        assert!(err2.contains("Invalid package name"), "got: {}", err2);
    }

    #[test]
    fn parse_npm_spec_rejects_empty_input() {
        let err = parse_npm_spec("").unwrap_err();
        assert!(err.contains("Empty package spec"), "got: {}", err);
    }
}
