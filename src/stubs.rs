//! Stub resolution — user-overridable code-generation templates.
//!
//! AdonisJS ships every `make:` template as a `.stub` file and looks in the
//! application's own stubs directory FIRST, falling back to the package's
//! built-in copy ("Searches in publishTarget first, then … package
//! locations"). An app publishes the stubs it wants to change and edits them;
//! everything else keeps using the defaults.
//!
//! One named deviation, forced by this generator being a Rust binary:
//! **substitution, not a template engine.** Adonis renders stubs with tempura
//! (`{{#var}}`, conditionals, partials); shipping that would mean shipping a JS
//! runtime inside the CLI. Here a stub is plain text with `{{ variable }}`
//! placeholders.
//!
//! Everything else follows Adonis, including the part that matters most: **a
//! stub chooses its own destination.** An Adonis stub opens with
//! `{{{ exports({ to: … }) }}}`; since that line is JavaScript we cannot
//! evaluate, the same declaration is written as front matter:
//!
//! ```text
//! ---
//! to: app/{{ module }}/{{ className }}.ts
//! ---
//! export class {{ className }} {}
//! ```
//!
//! Omit the front matter and the generator's default path is used, so a stub
//! published before this existed keeps working. The declared path still goes
//! through the same validation every generated path does — no absolute paths,
//! no `..` — which is what `app.httpControllersPath()` enforces on the Adonis
//! side too.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Where an app publishes its own stubs, mirroring Adonis' `stubs/` root.
pub const STUBS_DIR: &str = "stubs/make";

/// Reject a kind that could escape `stubs/make` — it reaches the filesystem.
fn is_safe_kind(kind: &str) -> bool {
    !kind.is_empty()
        && kind.len() <= 64
        && kind
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The path a stub for `kind` would live at, relative to the project root.
pub fn stub_path(kind: &str) -> Option<PathBuf> {
    if !is_safe_kind(kind) {
        return None;
    }
    let path = Path::new(STUBS_DIR).join(format!("{kind}.stub"));
    // Belt and braces: the kind charset already forbids these.
    if path
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::RootDir))
    {
        return None;
    }
    Some(path)
}

/// Read the app's stub for `kind`, or `None` when it has not published one.
pub fn read_override(kind: &str) -> Option<String> {
    let path = stub_path(kind)?;
    match fs::read_to_string(&path) {
        Ok(content) if !content.trim().is_empty() => Some(content),
        // An unreadable or empty stub falls back to the built-in rather than
        // generating an empty file — the failure mode nobody notices until the
        // generated module is imported.
        _ => None,
    }
}

/// Replace every `{{ name }}` placeholder. Unknown placeholders are left as
/// they are: a typo stays visible in the generated file instead of silently
/// becoming an empty string.
pub fn render(template: &str, vars: &BTreeMap<&str, String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            // Unclosed placeholder: emit the rest verbatim.
            out.push_str(&rest[start..]);
            return out;
        };
        let key = after[..end].trim();
        match vars.get(key) {
            Some(value) => out.push_str(value),
            None => {
                out.push_str("{{");
                out.push_str(&after[..end]);
                out.push_str("}}");
            }
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

/// A stub split into its optional `to:` declaration and its body.
#[derive(Debug)]
struct FrontMatter {
    to: Option<String>,
    body: String,
}

/// Parse the leading `--- … ---` block. Only `to:` is understood; any other
/// key is an error rather than a silent no-op, so a typo does not look like it
/// worked.
fn split_front_matter(template: &str) -> Result<FrontMatter, String> {
    let trimmed = template.trim_start_matches(['\u{feff}', '\n', '\r']);
    if !trimmed.starts_with("---") {
        return Ok(FrontMatter {
            to: None,
            body: template.to_string(),
        });
    }
    let after = &trimmed[3..];
    let after = after
        .strip_prefix('\n')
        .or_else(|| after.strip_prefix("\r\n"))
        .unwrap_or(after);
    let Some(end) = after.find("\n---") else {
        return Err("stub front matter opened with `---` but never closed".to_string());
    };
    let block = &after[..end];
    let rest = &after[end + 4..];
    let body = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))
        .unwrap_or(rest);

    let mut to = None;
    for line in block.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            return Err(format!("unreadable stub front-matter line: `{line}`"));
        };
        match key.trim() {
            "to" => to = Some(value.trim().to_string()),
            other => {
                return Err(format!(
                    "unknown stub front-matter key `{other}` (only `to` is supported)"
                ))
            }
        }
    }
    Ok(FrontMatter {
        to,
        body: body.to_string(),
    })
}

/// Reject a destination that would escape the project — the same rule the
/// generator applies to its own paths.
fn validate_destination(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("stub `to:` is empty".to_string());
    }
    let p = Path::new(path);
    if p.is_absolute() {
        return Err(format!(
            "stub `to:` must be a project-relative path, got `{path}`"
        ));
    }
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(format!("stub `to:` must not contain `..`, got `{path}`"));
    }
    Ok(())
}

/// What a stub produced: where to write it and what to write.
pub struct Resolved {
    pub path: String,
    pub content: String,
}

/// Resolve `kind` against the app's published stub, falling back to the
/// built-in. A stub may redirect its own output through `to:` front matter.
pub fn resolve(
    kind: &str,
    vars: &BTreeMap<&str, String>,
    default_path: String,
    built_in: String,
) -> Result<Resolved, String> {
    let Some(template) = read_override(kind) else {
        return Ok(Resolved {
            path: default_path,
            content: built_in,
        });
    };
    let front = split_front_matter(&template).map_err(|e| {
        format!(
            "{}: {e}",
            stub_path(kind)
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        )
    })?;
    let path = match front.to {
        Some(raw) => {
            let rendered = render(&raw, vars);
            validate_destination(&rendered)?;
            rendered
        }
        None => default_path,
    };
    Ok(Resolved {
        path,
        content: render(&front.body, vars),
    })
}

/// The kinds an app can publish a stub for.
///
/// The variables each one exposes are NOT listed here: they are read off the
/// template itself (`generator::stub_variables`), so `stubs:publish --list`
/// cannot advertise a name the template does not substitute — which a second,
/// hand-maintained copy did.
pub const PUBLISHABLE: &[&str] = &[
    "controller",
    "entity",
    "service",
    "validator",
    "seeder",
    "provider",
    "command",
    "middleware",
    "event",
];

/// `stubs:publish` — write the built-in template for `kind` into the app's
/// `stubs/make/` so it can be edited (Adonis' stub publishing). Generators
/// then prefer the published copy.
pub fn publish(
    kind: Option<&str>,
    built_in: &dyn Fn(&str) -> Option<String>,
    force: bool,
) -> Result<(), String> {
    let kinds: Vec<&str> = match kind {
        Some(k) => {
            if !PUBLISHABLE.contains(&k) {
                return Err(format!(
                    "unknown stub '{k}' (known: {})",
                    PUBLISHABLE.join(", ")
                ));
            }
            vec![k]
        }
        None => PUBLISHABLE.to_vec(),
    };

    fs::create_dir_all(STUBS_DIR).map_err(|e| format!("cannot create {STUBS_DIR}: {e}"))?;
    for k in kinds {
        let Some(path) = stub_path(k) else {
            return Err(format!("unsafe stub name '{k}'"));
        };
        if path.exists() && !force {
            println!(
                "  skipped {} (already published; --force to overwrite)",
                path.display()
            );
            continue;
        }
        let Some(content) = built_in(k) else {
            return Err(format!("no built-in template for '{k}'"));
        };
        fs::write(&path, content).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        println!("  wrote {}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&'static str, &str)]) -> BTreeMap<&'static str, String> {
        pairs.iter().map(|(k, v)| (*k, (*v).to_string())).collect()
    }

    #[test]
    fn substitutes_known_placeholders() {
        let out = render(
            "class {{ className }} in {{ module }}",
            &vars(&[("className", "Invoice"), ("module", "billing")]),
        );
        assert_eq!(out, "class Invoice in billing");
    }

    #[test]
    fn tolerates_spacing_inside_the_braces() {
        let out = render(
            "{{className}}|{{  className  }}",
            &vars(&[("className", "X")]),
        );
        assert_eq!(out, "X|X");
    }

    #[test]
    fn leaves_an_unknown_placeholder_visible() {
        // Silently emptying it would produce a file that looks fine and is not.
        let out = render("{{ nope }}", &vars(&[("className", "X")]));
        assert_eq!(out, "{{ nope }}");
    }

    #[test]
    fn emits_an_unclosed_placeholder_verbatim() {
        let out = render("start {{ oops", &vars(&[("oops", "X")]));
        assert_eq!(out, "start {{ oops");
    }

    #[test]
    fn refuses_a_kind_that_would_escape_the_stubs_directory() {
        assert!(stub_path("../../etc/passwd").is_none());
        assert!(stub_path("/etc/passwd").is_none());
        assert!(stub_path("Controller").is_none());
        assert!(stub_path("controller").is_some());
    }

    #[test]
    fn falls_back_to_the_built_in_when_nothing_is_published() {
        let out = resolve(
            "definitely-not-published",
            &vars(&[]),
            "app/Default.ts".to_string(),
            "built-in".to_string(),
        )
        .expect("resolve");
        assert_eq!(out.content, "built-in");
        assert_eq!(out.path, "app/Default.ts");
    }

    #[test]
    fn front_matter_is_optional() {
        let front = split_front_matter("export class X {}").expect("parse");
        assert_eq!(front.to, None);
        assert_eq!(front.body, "export class X {}");
    }

    #[test]
    fn a_stub_can_declare_where_it_writes() {
        let front =
            split_front_matter("---\nto: app/{{ module }}/X.ts\n---\nbody\n").expect("parse");
        assert_eq!(front.to.as_deref(), Some("app/{{ module }}/X.ts"));
        assert_eq!(front.body, "body\n");
    }

    #[test]
    fn an_unknown_front_matter_key_is_an_error() {
        // Ignoring it would look like the stub worked while the key did nothing.
        let err = split_front_matter("---\nfrom: x\n---\nbody").unwrap_err();
        assert!(err.contains("unknown stub front-matter key"), "{err}");
    }

    #[test]
    fn unclosed_front_matter_is_an_error() {
        let err = split_front_matter("---\nto: a.ts\nbody").unwrap_err();
        assert!(err.contains("never closed"), "{err}");
    }

    #[test]
    fn a_destination_cannot_escape_the_project() {
        assert!(validate_destination("../outside.ts").is_err());
        assert!(validate_destination("/etc/passwd").is_err());
        assert!(validate_destination("").is_err());
        assert!(validate_destination("app/orders/X.ts").is_ok());
    }
}
