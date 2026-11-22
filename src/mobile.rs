//! `ream android:build` and `ream android:run` — the app's Android build.
//!
//! AdonisJS has no counterpart (its assembler builds servers); the reference
//! is Expo. This does what `expo prebuild`, `expo run:android` and EAS Build
//! do, from the project's `reamrc.ts`:
//!
//! - the Android project is generated into `.ream/android` from the `mobile`
//!   config (name, id, version, icon, permissions, signing) and from the
//!   native parts its dependencies declare (`reamNative` in their
//!   package.json: platform sources, CMake, C++ modules, specs, Rust crates).
//!   It is regenerated on every run and never committed;
//! - the Rust crates are cross-compiled for the phone's ABIs with the NDK's
//!   linker, found from the environment or the SDK;
//! - Gradle builds and signs it: release builds with the app's keystore
//!   (passwords from the environment), debug builds with the standard debug
//!   key;
//! - `android:run` installs it over adb and starts it; a debug build loads its
//!   JS from Metro, which `android:run` serves, so a change to the JS reloads
//!   the app without a rebuild.

use std::collections::{BTreeSet, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;

use base64::Engine;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::tools::find_package;

// ---------------------------------------------------------------- config

const ABIS: [&str; 4] = ["arm64-v8a", "x86_64", "armeabi-v7a", "x86"];
const DEFAULT_ABIS: [&str; 2] = ["arm64-v8a", "x86_64"];
const MIN_SDK: u32 = 24;

#[derive(Debug, PartialEq)]
pub struct Signing {
    pub keystore: PathBuf,
    pub alias: String,
}

/// Over-the-air updates (@c9up/robledo): where the app asks, the key it
/// checks updates against, and where `android:publish` writes them.
#[derive(Debug, PartialEq)]
pub struct Updates {
    pub url: String,
    pub public_key: String,
    pub directory: PathBuf,
}

#[derive(Debug, PartialEq)]
pub struct MobileConfig {
    pub name: String,
    pub id: String,
    pub entry: PathBuf,
    pub version: String,
    pub build: u32,
    pub icon: Option<PathBuf>,
    pub permissions: Vec<String>,
    pub abis: Vec<String>,
    pub signing: Option<Signing>,
    pub updates: Option<Updates>,
    /// The app's Tailwind entry for the phone (`mobile.css`); see `styles`.
    pub css: Option<PathBuf>,
}

fn string(value: &Value, key: &str) -> Result<Option<String>, String> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("mobile.{key} must be a string")),
    }
}

fn required(value: &Value, key: &str) -> Result<String, String> {
    string(value, key)?
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("mobile.{key} is required in reamrc.ts"))
}

/// A Java package name: dot-separated, each part a letter then letters,
/// digits or underscores. Android also refuses a single part.
fn valid_id(id: &str) -> bool {
    let parts: Vec<&str> = id.split('.').collect();
    parts.len() >= 2
        && parts.iter().all(|part| {
            let mut chars = part.chars();
            chars.next().is_some_and(|c| c.is_ascii_alphabetic())
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// `CAMERA` → `android.permission.CAMERA`; a dotted name is kept as given.
fn permission(name: &str) -> Result<String, String> {
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    if !ok {
        return Err(format!(
            "mobile.android.permissions: {name:?} is not a permission name"
        ));
    }
    Ok(if name.contains('.') {
        name.to_string()
    } else {
        format!("android.permission.{name}")
    })
}

/// Read `mobile` from the rc file's JSON (see `read_config`).
pub fn parse_config(rc: &Value, root: &Path) -> Result<MobileConfig, String> {
    let mobile = rc
        .get("mobile")
        .filter(|m| m.is_object())
        .ok_or("reamrc.ts has no `mobile` block: add one with at least name, id and entry")?;
    let name = required(mobile, "name")?;
    let id = required(mobile, "id")?;
    if !valid_id(&id) {
        return Err(format!(
            "mobile.id {id:?} is not an application id: dot-separated parts, each starting with a letter (com.example.app)"
        ));
    }
    let entry = PathBuf::from(required(mobile, "entry")?);
    if !root.join(&entry).is_file() {
        return Err(format!("mobile.entry {} does not exist", entry.display()));
    }
    let version = string(mobile, "version")?.unwrap_or_else(|| "1.0.0".to_string());
    let build = match mobile.get("build") {
        None | Some(Value::Null) => 1,
        Some(v) => v
            .as_u64()
            .filter(|n| (1..=2_100_000_000).contains(n))
            .map(|n| n as u32)
            .ok_or("mobile.build must be a whole number from 1 (Android's versionCode)")?,
    };
    let icon = string(mobile, "icon")?.map(PathBuf::from);
    if let Some(icon) = &icon {
        if icon.extension().and_then(|e| e.to_str()) != Some("png") || !root.join(icon).is_file() {
            return Err(format!(
                "mobile.icon {} must be an existing PNG file",
                icon.display()
            ));
        }
    }
    let android = mobile.get("android").cloned().unwrap_or(Value::Null);
    let permissions = match android.get("permissions") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|p| {
                p.as_str()
                    .ok_or_else(|| {
                        format!("mobile.android.permissions: {p} is not a permission name")
                    })
                    .and_then(permission)
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err("mobile.android.permissions must be an array of names".into()),
    };
    let abis = match android.get("abis") {
        None | Some(Value::Null) => DEFAULT_ABIS.iter().map(|s| (*s).to_string()).collect(),
        Some(Value::Array(items)) => {
            let mut abis = Vec::new();
            for item in items {
                let abi = item.as_str().filter(|a| ABIS.contains(a)).ok_or_else(|| {
                    format!(
                        "mobile.android.abis: {item} is not one of {}",
                        ABIS.join(", ")
                    )
                })?;
                if !abis.iter().any(|a| a == abi) {
                    abis.push(abi.to_string());
                }
            }
            if abis.is_empty() {
                return Err("mobile.android.abis is empty".into());
            }
            abis
        }
        Some(_) => return Err("mobile.android.abis must be an array".into()),
    };
    let signing = match android.get("signing") {
        None | Some(Value::Null) => None,
        Some(s) if s.is_object() => Some(Signing {
            keystore: PathBuf::from(
                string(s, "keystore")?.ok_or("mobile.android.signing.keystore is required")?,
            ),
            alias: string(s, "alias")?.ok_or("mobile.android.signing.alias is required")?,
        }),
        Some(_) => return Err("mobile.android.signing must be { keystore, alias }".into()),
    };
    let updates = match mobile.get("updates") {
        None | Some(Value::Null) => None,
        Some(u) if u.is_object() => {
            let url = string(u, "url")?.ok_or("mobile.updates.url is required")?;
            if !update_url_allowed(&url) {
                return Err(format!(
                    "mobile.updates.url {url:?} must be https (plain http only to this computer: localhost, 127.0.0.1, 10.0.2.2)"
                ));
            }
            let public_key = string(u, "publicKey")?
                .ok_or("mobile.updates.publicKey is required (ream generate:update-key)")?;
            let decoded: [u8; 32] = base64::engine::general_purpose::STANDARD
                .decode(public_key.trim())
                .ok()
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or("mobile.updates.publicKey is not a base64 Ed25519 public key (ream generate:update-key prints one)")?;
            if ed25519_dalek::VerifyingKey::from_bytes(&decoded).is_err() {
                return Err("mobile.updates.publicKey is not a valid Ed25519 public key".into());
            }
            let directory =
                PathBuf::from(string(u, "directory")?.unwrap_or_else(|| "robledo".to_string()));
            Some(Updates {
                url: url.trim_end_matches('/').to_string(),
                public_key: public_key.trim().to_string(),
                directory,
            })
        }
        Some(_) => return Err("mobile.updates must be { url, publicKey }".into()),
    };
    let css = string(mobile, "css")?.map(PathBuf::from);
    if let Some(css) = &css {
        if !root.join(css).is_file() {
            return Err(format!("mobile.css {} does not exist", css.display()));
        }
    }
    Ok(MobileConfig {
        name,
        id,
        entry,
        version,
        build,
        icon,
        permissions,
        abis,
        signing,
        updates,
        css,
    })
}

/// An update server over https, or plain http to the computer the phone is
/// plugged into (adb reverse) or an emulator's host. The updates are signed
/// either way; https keeps who asks for what private.
fn update_url_allowed(url: &str) -> bool {
    if url.starts_with("https://") && url.len() > "https://".len() {
        return true;
    }
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split(['/', ':']).next().unwrap_or("");
    matches!(host, "localhost" | "127.0.0.1" | "10.0.2.2")
}

/// The host of a plain-http update URL (one `update_url_allowed` let
/// through), which the app must be allowed to reach in clear.
fn cleartext_update_host(config: &MobileConfig) -> Option<String> {
    let rest = config.updates.as_ref()?.url.strip_prefix("http://")?;
    rest.split(['/', ':']).next().map(str::to_string)
}

/// The rc file's `mobile` block. Node reads it: `reamrc.ts` is TypeScript
/// (see `commands::read_rc`).
pub fn read_config(root: &Path) -> Result<MobileConfig, String> {
    if !root.join("reamrc.ts").exists() {
        return Err("reamrc.ts not found: run this from a ream project".into());
    }
    let output = Command::new("node")
        .current_dir(root)
        .args([
            "--import",
            "@swc-node/register/esm-register",
            "--input-type=module",
            "-e",
            "const rc = (await import('./reamrc.ts')).default; \
             process.stdout.write(JSON.stringify({ mobile: rc?.mobile ?? null }));",
        ])
        .output()
        .map_err(|e| format!("Failed to read reamrc.ts: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "Could not read reamrc.ts:\n{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let rc: Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("reamrc.ts did not yield readable config: {e}"))?;
    parse_config(&rc, root)
}

// ---------------------------------------------------------------- native packages

#[derive(Debug, Default)]
pub struct CxxModule {
    pub name: String,
    pub header: String,
}

/// What a dependency declares in `reamNative` (orbit's package.json is the
/// reference): paths are resolved against the package.
#[derive(Debug, Default)]
pub struct NativePackage {
    pub name: String,
    pub version: String,
    /// A static `(Context) -> String?` that picks the JS bundle to launch
    /// (`reamNative.android.bundleProvider`, as `pkg.Class.method`).
    pub bundle_provider: Option<String>,
    /// A static `(Application) -> Unit` the app calls when it starts, before
    /// React Native loads (`reamNative.android.initializer`, as
    /// `pkg.Class.method`): where a package's native code gets the app's
    /// `Context` when it has no view or module to receive it.
    pub initializer: Option<String>,
    pub specs: Option<PathBuf>,
    /// (Cargo.toml, library name)
    pub rust: Vec<(PathBuf, String)>,
    pub java: Option<PathBuf>,
    /// (CMake file, the function that links it into a target)
    pub cmake: Option<(PathBuf, String)>,
    pub cxx_modules: Vec<CxxModule>,
}

fn package_json(dir: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(dir.join("package.json")).ok()?).ok()
}

fn dependency_names(pkg: &Value) -> Vec<String> {
    let mut names = BTreeSet::new();
    for field in ["dependencies", "peerDependencies", "optionalDependencies"] {
        if let Some(map) = pkg.get(field).and_then(Value::as_object) {
            names.extend(map.keys().cloned());
        }
    }
    names.into_iter().collect()
}

/// A path the package declares, which must stay inside it.
fn inside(root: &Path, relative: &str, what: &str, package: &str) -> Result<PathBuf, String> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!(
            "{package}: reamNative.{what} must be a path inside the package"
        ));
    }
    Ok(root.join(relative))
}

/// A `package.Class.method` a package names for the app to call: written
/// into the generated Kotlin, so nothing but identifiers.
fn static_method(package: &str, what: &str, value: String) -> Result<String, String> {
    let parts: Vec<&str> = value.split('.').collect();
    let identifier = |s: &&str| {
        let mut chars = s.chars();
        chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    if parts.len() < 3 || !parts.iter().all(identifier) {
        return Err(format!(
            "{package}: reamNative.android.{what} {value:?} is not package.Class.method"
        ));
    }
    Ok(value)
}

fn native_of(name: &str, root: &Path, pkg: &Value) -> Result<Option<NativePackage>, String> {
    let Some(native) = pkg.get("reamNative") else {
        return Ok(None);
    };
    let text = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).map(str::to_string);
    let mut out = NativePackage {
        name: name.to_string(),
        version: pkg
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("0.0.0")
            .to_string(),
        ..NativePackage::default()
    };
    if let Some(specs) = text(native, "specs") {
        out.specs = Some(inside(root, &specs, "specs", name)?);
    }
    for krate in native
        .get("rust")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let manifest = text(krate, "manifest")
            .ok_or_else(|| format!("{name}: reamNative.rust[].manifest is required"))?;
        let lib = text(krate, "lib")
            .ok_or_else(|| format!("{name}: reamNative.rust[].lib is required"))?;
        if !lib.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!(
                "{name}: reamNative.rust[].lib {lib:?} is not a library name"
            ));
        }
        out.rust
            .push((inside(root, &manifest, "rust[].manifest", name)?, lib));
    }
    if let Some(android) = native.get("android") {
        if let Some(java) = text(android, "java") {
            out.java = Some(inside(root, &java, "android.java", name)?);
        }
        if let Some(cmake) = android.get("cmake") {
            let file = text(cmake, "file")
                .ok_or_else(|| format!("{name}: reamNative.android.cmake.file is required"))?;
            let link = text(cmake, "link")
                .ok_or_else(|| format!("{name}: reamNative.android.cmake.link is required"))?;
            if !link.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return Err(format!(
                    "{name}: reamNative.android.cmake.link {link:?} is not a CMake function name"
                ));
            }
            out.cmake = Some((inside(root, &file, "android.cmake.file", name)?, link));
        }
        if let Some(provider) = text(android, "bundleProvider") {
            out.bundle_provider = Some(static_method(name, "bundleProvider", provider)?);
        }
        if let Some(initializer) = text(android, "initializer") {
            out.initializer = Some(static_method(name, "initializer", initializer)?);
        }
        for module in android
            .get("cxxModules")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let module_name = text(module, "name").ok_or_else(|| {
                format!("{name}: reamNative.android.cxxModules[].name is required")
            })?;
            let header = text(module, "header").ok_or_else(|| {
                format!("{name}: reamNative.android.cxxModules[].header is required")
            })?;
            let identifier =
                |s: &str| s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !s.is_empty();
            let header_ok = header
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "_./-".contains(c))
                && !header.contains("..");
            if !identifier(&module_name) || !header_ok {
                return Err(format!("{name}: reamNative.android.cxxModules[] {module_name:?} / {header:?} is not a C++ class and header"));
            }
            out.cxx_modules.push(CxxModule {
                name: module_name,
                header,
            });
        }
    }
    Ok(Some(out))
}

/// Every installed dependency, direct or not, that declares native parts,
/// in a stable order (by name).
pub fn native_packages(root: &Path) -> Result<Vec<NativePackage>, String> {
    let app = package_json(root).ok_or("package.json not found or unreadable")?;
    let mut queue: VecDeque<(String, PathBuf)> = dependency_names(&app)
        .into_iter()
        .map(|n| (n, root.to_path_buf()))
        .collect();
    let mut seen = HashSet::new();
    let mut found = Vec::new();
    while let Some((name, from)) = queue.pop_front() {
        let Some(dir) = find_package(&from, &name) else {
            continue;
        };
        let dir = dir.canonicalize().unwrap_or(dir);
        if !seen.insert(dir.clone()) {
            continue;
        }
        let Some(pkg) = package_json(&dir) else {
            continue;
        };
        if let Some(native) = native_of(&name, &dir, &pkg)? {
            found.push(native);
        }
        queue.extend(dependency_names(&pkg).into_iter().map(|n| (n, dir.clone())));
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(found)
}

/// The native runtime an update must match: React Native's version and every
/// native package's name and version. JS built against another set may call
/// native code this build lacks, so a phone refuses it and waits for a store
/// update (@c9up/robledo checks it; `android:publish` stamps it).
pub fn runtime_version(layout: &Layout, natives: &[NativePackage]) -> String {
    let rn = package_json(&layout.node_modules.join("react-native"))
        .and_then(|p| p.get("version").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default();
    let mut parts = vec![format!("react-native@{rn}")];
    let mut packages: Vec<String> = natives
        .iter()
        .map(|n| format!("{}@{}", n.name, n.version))
        .collect();
    packages.sort();
    parts.extend(packages);
    let digest = Sha256::digest(parts.join("\n").as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------- generation

const T_SETTINGS: &str = include_str!("mobile/android/settings.gradle");
const T_BUILD: &str = include_str!("mobile/android/build.gradle");
const T_PROPERTIES: &str = include_str!("mobile/android/gradle.properties");
const T_PACKAGE: &str = include_str!("mobile/android/package.json");
const T_APP: &str = include_str!("mobile/android/app.gradle");
const T_MANIFEST: &str = include_str!("mobile/android/AndroidManifest.xml");
const T_ACTIVITY: &str = include_str!("mobile/android/MainActivity.kt");
const T_APPLICATION: &str = include_str!("mobile/android/MainApplication.kt");
const T_CMAKE: &str = include_str!("mobile/android/CMakeLists.txt");
const T_ONLOAD: &str = include_str!("mobile/android/OnLoad.cpp");
const T_STRINGS: &str = include_str!("mobile/android/strings.xml");
const T_STYLES: &str = include_str!("mobile/android/styles.xml");
const T_METRO: &str = include_str!("mobile/android/metro.config.cjs");
const T_RN_CONFIG: &str = include_str!("mobile/android/react-native.config.js");
const T_PROGUARD: &str = include_str!("mobile/android/proguard-rules.pro");
const T_GRADLEW: &str = include_str!("mobile/android/gradlew");
const T_GRADLEW_BAT: &str = include_str!("mobile/android/gradlew.bat");
const T_WRAPPER_PROPERTIES: &str = include_str!("mobile/android/gradle/gradle-wrapper.properties");
const B_WRAPPER_JAR: &[u8] = include_bytes!("mobile/android/gradle/gradle-wrapper.jar");
const B_DEBUG_KEYSTORE: &[u8] = include_bytes!("mobile/android/debug.keystore");
const B_EDIT_TEXT: &[u8] = include_bytes!("mobile/android/res/rn_edit_text_material.xml");
const B_ICON: &[u8] = include_bytes!("mobile/android/res/ic_launcher.png");
const B_ICON_ROUND: &[u8] = include_bytes!("mobile/android/res/ic_launcher_round.png");

/// `{{key}}` → value; a key left unfilled is a bug in this file.
fn fill(template: &str, values: &[(&str, String)]) -> String {
    let mut out = template.to_string();
    for (key, value) in values {
        out = out.replace(&format!("{{{{{key}}}}}"), value);
    }
    debug_assert!(!out.contains("{{"), "unfilled template key in:\n{out}");
    out
}

/// The entry as Metro names it: relative, forward slashes, no extension.
fn main_module(entry: &Path) -> String {
    gradle_path(&entry.with_extension(""))
}

/// A path as Gradle and CMake take it: forward slashes, quotes escaped.
fn gradle_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .replace('"', "\\\"")
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "\\'")
}

fn write(path: &Path, contents: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    // Rewritten only when it changed, so Gradle's up-to-date checks hold.
    if std::fs::read(path).ok().as_deref() == Some(contents) {
        return Ok(());
    }
    std::fs::write(path, contents).map_err(|e| format!("{}: {e}", path.display()))
}

/// The outermost directory above `root` (or `root` itself) that is a
/// workspace root: a `pnpm-workspace.yaml`, or a package.json declaring
/// `workspaces`. Without one, `root`.
fn workspace_root(root: &Path) -> PathBuf {
    root.ancestors()
        .filter(|dir| {
            dir.join("pnpm-workspace.yaml").is_file()
                || package_json(dir).is_some_and(|p| p.get("workspaces").is_some())
        })
        .last()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| root.to_path_buf())
}

/// Metro's configuration for the app: its entry from the project root, the
/// workspace watched, the export conditions our packages publish.
fn metro_config(layout: &Layout) -> String {
    fill(
        T_METRO,
        &[(
            "workspaceRoot",
            serde_json::to_string(&layout.workspace.to_string_lossy()).unwrap_or_default(),
        )],
    )
}

/// The directories `ream android:*` writes under the project.
pub struct Layout {
    pub root: PathBuf,
    pub ream: PathBuf,
    pub android: PathBuf,
    pub rust: PathBuf,
    /// The directory react-native really sits in (its link followed): its own
    /// dependencies — the Gradle plugin, codegen, Hermes' compiler — are
    /// there, which an app under pnpm's isolated linker does not see.
    pub node_modules: PathBuf,
    /// The app's React Native CLI (autolinking runs it).
    pub community_cli: Option<PathBuf>,
    /// The outermost workspace the app belongs to (a pnpm or npm/yarn
    /// workspace root), else the app: what Metro watches, so packages linked
    /// from elsewhere in the monorepo resolve.
    pub workspace: PathBuf,
}

impl Layout {
    pub fn new(root: &Path) -> Result<Layout, String> {
        let root = root
            .canonicalize()
            .map_err(|e| format!("{}: {e}", root.display()))?;
        let rn = find_package(&root, "react-native")
            .ok_or("react-native is not installed: add it to the app's dependencies")?;
        // The node_modules really holding react-native: its tooling lives beside it.
        let rn = rn.canonicalize().unwrap_or(rn);
        let node_modules = rn
            .parent()
            .map(Path::to_path_buf)
            .ok_or("react-native has no parent directory")?;
        let ream = root.join(".ream");
        Ok(Layout {
            android: ream.join("android"),
            rust: ream.join("rust"),
            community_cli: find_package(&root, "@react-native-community/cli"),
            workspace: workspace_root(&root),
            ream,
            root,
            node_modules,
        })
    }

    /// What the build needs and the app does not have, as one message naming
    /// the install that fixes it — before anything is generated or compiled,
    /// rather than from deep inside Gradle.
    pub fn check_tooling(&self) -> Result<(), String> {
        let rn = package_json(&self.node_modules.join("react-native"))
            .and_then(|p| p.get("version").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default();
        let mut missing = Vec::new();
        for (package, what) in [
            ("@react-native/gradle-plugin", "the Gradle plugin"),
            ("@react-native/codegen", "codegen"),
        ] {
            if !self
                .node_modules
                .join(package)
                .join("package.json")
                .is_file()
            {
                missing.push(format!("{package}@{rn} ({what})"));
            }
        }
        if self.community_cli.is_none() {
            missing.push("@react-native-community/cli (autolinking)".to_string());
        }
        if find_package(&self.root, "@react-native/metro-config").is_none() {
            missing.push(format!("@react-native/metro-config@{rn} (Metro)"));
        }
        // Babel injects its helpers into every module it transpiles; React
        // Native's preset wants the 7.x line (a bare `pnpm add` gives 8).
        let runtime = find_package(&self.root, "@babel/runtime")
            .and_then(|dir| package_json(&dir))
            .and_then(|p| p.get("version").and_then(Value::as_str).map(str::to_string));
        if !runtime.is_some_and(|v| v.starts_with("7.")) {
            missing.push("@babel/runtime@7 (Babel's helpers)".to_string());
        }
        if missing.is_empty() {
            return Ok(());
        }
        let names = missing
            .iter()
            .map(|m| m.split(' ').next().unwrap_or(m).to_string())
            .collect::<Vec<_>>()
            .join(" ");
        Err(format!(
            "The Android build needs packages this app does not have:\n  - {}\nInstall them: pnpm add -D {names}",
            missing.join("\n  - ")
        ))
    }
}

/// Write `.ream/android` for `config` and the native packages.
/// The app's styles for the phone: Tailwind over the app, as its web build
/// runs it, compiled into the table orbit reads (`.ream/orbit-styles.json`,
/// which Metro serves as `@c9up/orbit-styles`). The entry is `mobile.css`, else
/// `@import "tailwindcss"` scanning the mobile entry's directory. Without
/// Tailwind installed the table is empty: every class is reported unknown.
fn styles(layout: &Layout, config: &MobileConfig) -> Result<(), String> {
    let out = layout.ream.join("orbit-styles.json");
    // The app's own Tailwind, from its node_modules (not react-native's).
    let bin = crate::tools::local_bin(&layout.root, "tailwindcss")
        .unwrap_or_else(|| layout.root.join("node_modules/.bin/tailwindcss"));
    if !bin.exists() {
        if config.css.is_some() {
            return Err("mobile.css is set but Tailwind is not installed: pnpm add -D tailwindcss @tailwindcss/cli".into());
        }
        crate::ui::warning(
            "Tailwind is not installed: classes will not style the app (pnpm add -D tailwindcss @tailwindcss/cli)",
        );
        let empty = serde_json::json!({ "version": 1, "properties": {}, "rules": [], "inert": [] });
        return write(&out, empty.to_string().as_bytes());
    }
    let input = match &config.css {
        Some(css) => layout.root.join(css),
        None => {
            let dir = config.entry.parent().unwrap_or(Path::new("."));
            let source = Path::new("..").join(dir);
            let entry = layout.ream.join("orbit-input.css");
            write(
                &entry,
                format!(
                    "/* Generated by `ream android:*`: Tailwind over the mobile app (set mobile.css to use your own). */\n@import \"tailwindcss\";\n@source \"{}\";\n",
                    source.display().to_string().replace('\\', "/")
                )
                .as_bytes(),
            )?;
            entry
        }
    };
    let compiled = layout.ream.join("orbit.css");
    crate::ui::info("Tailwind");
    let result = Command::new(&bin)
        .current_dir(&layout.root)
        .arg("-i")
        .arg(&input)
        .arg("-o")
        .arg(&compiled)
        .output()
        .map_err(|e| format!("Tailwind: {e}"))?;
    if !result.status.success() {
        return Err(format!(
            "Tailwind failed on {}:\n{}",
            input.display(),
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    let css =
        std::fs::read_to_string(&compiled).map_err(|e| format!("{}: {e}", compiled.display()))?;
    write(&out, crate::css::compile(&css).to_string().as_bytes())
}

pub fn generate(
    layout: &Layout,
    config: &MobileConfig,
    natives: &[NativePackage],
    ndk_version: &str,
) -> Result<(), String> {
    if !natives.iter().any(|n| n.name == "@c9up/orbit") {
        return Err("@c9up/orbit is not installed: a ream mobile app renders through it".into());
    }
    let providers: Vec<(&str, &str)> = natives
        .iter()
        .filter_map(|n| n.bundle_provider.as_deref().map(|p| (n.name.as_str(), p)))
        .collect();
    if providers.len() > 1 {
        return Err(format!(
            "Several packages pick the JS bundle to launch ({}): keep one",
            providers
                .iter()
                .map(|(n, _)| *n)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if config.updates.is_some() && providers.is_empty() {
        return Err(
            "mobile.updates is set but no installed package applies updates: add @c9up/robledo"
                .into(),
        );
    }
    // Called in every build so the package can open its state; its choice
    // is used in release builds only (a debug build loads its JS from Metro).
    let bundle_file = providers
        .first()
        .map(|(name, provider)| {
            format!(
                "      // {name}: the bundle to launch, an update or the app's own.\n      jsBundleFilePath = {provider}(applicationContext).takeUnless {{ BuildConfig.DEBUG }},\n"
            )
        })
        .unwrap_or_default();
    // Each package's start hook, in the packages' order, before React Native loads.
    let initializers = natives
        .iter()
        .filter_map(|n| {
            n.initializer
                .as_deref()
                .map(|method| format!("    // {}\n    {method}(this)\n", n.name))
        })
        .collect::<String>();
    let a = &layout.android;
    let gradle_name: String = config
        .name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let id_dir = config.id.replace('.', "/");
    let release = config.signing.as_ref().map(|s| {
        (
            format!(
                "        release {{\n            storeFile file(\"{}\")\n            storePassword System.getenv(\"REAM_ANDROID_STORE_PASSWORD\")\n            keyAlias \"{}\"\n            keyPassword System.getenv(\"REAM_ANDROID_KEY_PASSWORD\")\n        }}\n",
                gradle_path(&layout.root.join(&s.keystore)),
                s.alias.replace('"', "\\\""),
            ),
            "            signingConfig signingConfigs.release\n".to_string(),
        )
    });
    let java_dirs = natives
        .iter()
        .filter_map(|n| n.java.as_ref())
        .map(|p| format!("\"{}\"", gradle_path(p)))
        .collect::<Vec<_>>()
        .join(", ");
    let values = [
        ("projectRoot", gradle_path(&layout.root)),
        ("reamDir", gradle_path(&layout.ream)),
        ("nodeModules", gradle_path(&layout.node_modules)),
        (
            "communityCli",
            layout
                .community_cli
                .as_deref()
                .map(gradle_path)
                .unwrap_or_default(),
        ),
        ("gradleName", gradle_name.clone()),
        ("minSdk", MIN_SDK.to_string()),
        ("ndkVersion", ndk_version.to_string()),
        ("abis", config.abis.join(",")),
        ("version", config.version.replace('"', "")),
        ("id", config.id.clone()),
        ("build", config.build.to_string()),
        ("entry", gradle_path(&layout.root.join(&config.entry))),
        ("mainModule", main_module(&config.entry)),
        ("bundleFile", bundle_file),
        ("initializers", initializers),
        (
            "networkSecurity",
            if cleartext_update_host(config).is_some() {
                "\n      android:networkSecurityConfig=\"@xml/ream_network_security\"".to_string()
            } else {
                String::new()
            },
        ),
        ("metroConfig", gradle_path(&layout.ream.join("metro.config.cjs"))),
        ("javaDirs", java_dirs),
        ("rustDir", gradle_path(&layout.rust)),
        ("releaseSigning", release.as_ref().map(|r| r.0.clone()).unwrap_or_default()),
        ("releaseSigningUse", release.map(|r| r.1).unwrap_or_default()),
        (
            "permissions",
            config
                .permissions
                .iter()
                .map(|p| format!("    <uses-permission android:name=\"{}\" />\n", xml_escape(p)))
                .collect(),
        ),
        ("name", xml_escape(&config.name)),
        (
            "cmakeLinks",
            natives
                .iter()
                .filter_map(|n| n.cmake.as_ref().map(|c| (n, c)))
                .map(|(n, (file, link))| {
                    format!("# {}\ninclude(\"{}\")\n{link}(${{CMAKE_PROJECT_NAME}})\n", n.name, gradle_path(file))
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        (
            "cxxIncludes",
            natives
                .iter()
                .flat_map(|n| &n.cxx_modules)
                .map(|m| format!("#include <{}>\n", m.header))
                .collect(),
        ),
        (
            "cxxModules",
            natives
                .iter()
                .flat_map(|n| &n.cxx_modules)
                .map(|m| {
                    format!(
                        "  if (name == {0}::kModuleName) {{\n    return std::make_shared<{0}>(jsInvoker);\n  }}\n",
                        m.name
                    )
                })
                .collect(),
        ),
    ];
    let main = a.join("app/src/main");
    write(
        &a.join("settings.gradle"),
        fill(T_SETTINGS, &values).as_bytes(),
    )?;
    write(&a.join("build.gradle"), fill(T_BUILD, &values).as_bytes())?;
    write(
        &a.join("gradle.properties"),
        fill(T_PROPERTIES, &values).as_bytes(),
    )?;
    write(&a.join("package.json"), fill(T_PACKAGE, &values).as_bytes())?;
    write(&a.join("gradlew"), T_GRADLEW.as_bytes())?;
    write(&a.join("gradlew.bat"), T_GRADLEW_BAT.as_bytes())?;
    write(
        &a.join("gradle/wrapper/gradle-wrapper.properties"),
        T_WRAPPER_PROPERTIES.as_bytes(),
    )?;
    write(&a.join("gradle/wrapper/gradle-wrapper.jar"), B_WRAPPER_JAR)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(a.join("gradlew"), std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("gradlew: {e}"))?;
    }
    write(&a.join("app/build.gradle"), fill(T_APP, &values).as_bytes())?;
    write(&a.join("app/proguard-rules.pro"), T_PROGUARD.as_bytes())?;
    write(&a.join("app/debug.keystore"), B_DEBUG_KEYSTORE)?;
    write(
        &main.join("AndroidManifest.xml"),
        fill(T_MANIFEST, &values).as_bytes(),
    )?;
    // An id changed since the last run leaves the old package's activity behind.
    let java = main.join("java");
    if java.exists() && !java.join(&id_dir).join("MainActivity.kt").exists() {
        std::fs::remove_dir_all(&java).map_err(|e| format!("{}: {e}", java.display()))?;
    }
    write(
        &main.join(format!("java/{id_dir}/MainActivity.kt")),
        fill(T_ACTIVITY, &values).as_bytes(),
    )?;
    write(
        &main.join(format!("java/{id_dir}/MainApplication.kt")),
        fill(T_APPLICATION, &values).as_bytes(),
    )?;
    write(
        &main.join("jni/CMakeLists.txt"),
        fill(T_CMAKE, &values).as_bytes(),
    )?;
    write(
        &main.join("jni/OnLoad.cpp"),
        fill(T_ONLOAD, &values).as_bytes(),
    )?;
    write(
        &main.join("res/values/strings.xml"),
        fill(T_STRINGS, &values).as_bytes(),
    )?;
    write(&main.join("res/values/styles.xml"), T_STYLES.as_bytes())?;
    write(
        &main.join("res/drawable/rn_edit_text_material.xml"),
        B_EDIT_TEXT,
    )?;
    // One density: Android scales the icon for the others.
    let (icon, round) = match &config.icon {
        Some(path) => {
            let bytes = std::fs::read(layout.root.join(path))
                .map_err(|e| format!("{}: {e}", path.display()))?;
            (bytes.clone(), bytes)
        }
        None => (B_ICON.to_vec(), B_ICON_ROUND.to_vec()),
    };
    write(&main.join("res/mipmap-xxxhdpi/ic_launcher.png"), &icon)?;
    write(
        &main.join("res/mipmap-xxxhdpi/ic_launcher_round.png"),
        &round,
    )?;
    write(
        &layout.ream.join("metro.config.cjs"),
        metro_config(layout).as_bytes(),
    )?;
    styles(layout, config)?;
    // Android refuses plain http in a release build: an update server on
    // this computer (or the emulator's host) is let through, and only it.
    let security = main.join("res/xml/ream_network_security.xml");
    match cleartext_update_host(config) {
        Some(host) => {
            let xml = format!(
                "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<!-- Generated by `ream android:*`: plain http to the local update server only. -->\n<network-security-config>\n  <domain-config cleartextTrafficPermitted=\"true\">\n    <domain includeSubdomains=\"false\">{}</domain>\n  </domain-config>\n</network-security-config>\n",
                xml_escape(&host)
            );
            write(&security, xml.as_bytes())?;
        }
        None => {
            let _ = std::fs::remove_file(&security);
        }
    }
    // What the update package reads at the app's start (see @c9up/robledo).
    let updates_config = main.join("assets/ream-updates.json");
    match &config.updates {
        Some(updates) => {
            let value = serde_json::json!({
                "url": updates.url,
                "publicKey": updates.public_key,
                "runtimeVersion": runtime_version(layout, natives),
                "platform": "android",
            });
            write(&updates_config, value.to_string().as_bytes())?;
        }
        None => {
            let _ = std::fs::remove_file(&updates_config);
        }
    }
    write(
        &layout.ream.join("react-native.config.js"),
        fill(T_RN_CONFIG, &values).as_bytes(),
    )?;
    // React Native's CLI reads the dependencies to autolink from here.
    let app = package_json(&layout.root).unwrap_or(Value::Null);
    let mut dependencies = serde_json::Map::new();
    for field in ["dependencies", "devDependencies"] {
        if let Some(map) = app.get(field).and_then(Value::as_object) {
            dependencies.extend(map.clone());
        }
    }
    let cli_package = serde_json::json!({
        "name": format!("{gradle_name}-ream"),
        "private": true,
        "description": "Generated by `ream android:*`: the app's dependencies, for React Native's autolinking.",
        "dependencies": dependencies,
    });
    write(
        &layout.ream.join("package.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&cli_package).unwrap_or_default()
        )
        .as_bytes(),
    )?;
    // The specs React Native's codegen reads (package.json above names them
    // AppSpecs, the header orbit's C++ module includes).
    let specs = a.join("specs");
    if specs.exists() {
        std::fs::remove_dir_all(&specs).map_err(|e| format!("{}: {e}", specs.display()))?;
    }
    for native in natives {
        let Some(dir) = &native.specs else { continue };
        for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e == "ts" || e == "js")
            {
                let file = path.file_name().map(PathBuf::from).unwrap_or_default();
                let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                write(&specs.join(file), &bytes)?;
            }
        }
    }
    let ignore = layout.ream.join(".gitignore");
    if !ignore.exists() {
        write(&ignore, b"# Generated by ream: never committed.\n*\n")?;
    }
    Ok(())
}

// ---------------------------------------------------------------- NDK and Rust

/// `27.1.12297006` > `26.3.11579264`, part by part.
fn version_key(name: &str) -> Vec<u64> {
    name.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// The NDK to build with: ANDROID_NDK_HOME / ANDROID_NDK_ROOT, else the
/// newest under the SDK's `ndk/` (ANDROID_HOME, ANDROID_SDK_ROOT, or the
/// default SDK location).
pub fn find_ndk(
    env: &dyn Fn(&str) -> Option<String>,
    home: Option<PathBuf>,
) -> Result<PathBuf, String> {
    for key in ["ANDROID_NDK_HOME", "ANDROID_NDK_ROOT"] {
        if let Some(dir) = env(key).filter(|d| !d.is_empty()) {
            let dir = PathBuf::from(dir);
            if dir.is_dir() {
                return Ok(dir);
            }
            return Err(format!(
                "{key} is {} but no such directory exists",
                dir.display()
            ));
        }
    }
    let mut sdks: Vec<PathBuf> = ["ANDROID_HOME", "ANDROID_SDK_ROOT"]
        .iter()
        .filter_map(|k| env(k).filter(|d| !d.is_empty()).map(PathBuf::from))
        .collect();
    if let Some(home) = home {
        sdks.push(home.join("Android/Sdk"));
        sdks.push(home.join("Library/Android/sdk"));
    }
    for sdk in sdks {
        let Ok(entries) = std::fs::read_dir(sdk.join("ndk")) else {
            continue;
        };
        let newest = entries
            .filter_map(Result::ok)
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .max_by_key(|name| version_key(name));
        if let Some(name) = newest {
            return Ok(sdk.join("ndk").join(name));
        }
    }
    Err("No Android NDK found: install one with the SDK manager (ndk;27.1.12297006), or set ANDROID_NDK_HOME".into())
}

fn ndk_version(ndk: &Path) -> String {
    let props = std::fs::read_to_string(ndk.join("source.properties")).unwrap_or_default();
    props
        .lines()
        .find_map(|l| {
            l.strip_prefix("Pkg.Revision")
                .map(|r| r.trim_start_matches([' ', '=']).trim().to_string())
        })
        .or_else(|| ndk.file_name().and_then(|n| n.to_str()).map(str::to_string))
        .unwrap_or_default()
}

pub fn rust_triple(abi: &str) -> &'static str {
    match abi {
        "arm64-v8a" => "aarch64-linux-android",
        "armeabi-v7a" => "armv7-linux-androideabi",
        "x86" => "i686-linux-android",
        _ => "x86_64-linux-android",
    }
}

/// The NDK clang prefix differs from the Rust triple for 32-bit ARM.
fn clang_triple(abi: &str) -> &'static str {
    match abi {
        "armeabi-v7a" => "armv7a-linux-androideabi",
        other => rust_triple(other),
    }
}

fn ndk_host() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin-x86_64"
    } else if cfg!(target_os = "windows") {
        "windows-x86_64"
    } else {
        "linux-x86_64"
    }
}

/// Cross-compile each native package's Rust crates for each ABI, into
/// `.ream/rust/<abi>/lib<name>.a`, which the generated CMake links.
pub fn build_rust(
    layout: &Layout,
    natives: &[NativePackage],
    abis: &[String],
    ndk: &Path,
) -> Result<(), String> {
    let crates: Vec<&(PathBuf, String)> = natives.iter().flat_map(|n| &n.rust).collect();
    if crates.is_empty() {
        return Ok(());
    }
    let installed = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string());
    let bin = ndk
        .join("toolchains/llvm/prebuilt")
        .join(ndk_host())
        .join("bin");
    let ext = if cfg!(target_os = "windows") {
        ".cmd"
    } else {
        ""
    };
    for abi in abis {
        let triple = rust_triple(abi);
        if let Some(list) = &installed {
            if !list.lines().any(|l| l.trim() == triple) {
                return Err(format!(
                    "The Rust target {triple} is not installed: rustup target add {triple}"
                ));
            }
        }
        let env_triple = triple.to_uppercase().replace('-', "_");
        let linker = bin.join(format!("{}{MIN_SDK}-clang{ext}", clang_triple(abi)));
        let ar = bin.join(format!(
            "llvm-ar{}",
            if cfg!(target_os = "windows") {
                ".exe"
            } else {
                ""
            }
        ));
        for (manifest, lib) in &crates {
            crate::ui::info(&format!("Rust {lib} for {abi}"));
            let status = Command::new("cargo")
                .args(["build", "--release", "--target", triple, "--manifest-path"])
                .arg(manifest)
                .env("CARGO_TARGET_DIR", layout.rust.join("target"))
                .env(format!("CARGO_TARGET_{env_triple}_LINKER"), &linker)
                .env(format!("CC_{}", triple.replace('-', "_")), &linker)
                .env(format!("AR_{}", triple.replace('-', "_")), &ar)
                .status()
                .map_err(|e| format!("cargo: {e}"))?;
            if !status.success() {
                return Err(format!("Building {} for {abi} failed", manifest.display()));
            }
            let built = layout
                .rust
                .join("target")
                .join(triple)
                .join("release")
                .join(format!("lib{lib}.a"));
            let dest = layout.rust.join(abi).join(format!("lib{lib}.a"));
            std::fs::create_dir_all(dest.parent().unwrap_or(&layout.rust))
                .map_err(|e| e.to_string())?;
            std::fs::copy(&built, &dest).map_err(|e| format!("{}: {e}", built.display()))?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- build, install, run

pub struct BuildOptions {
    pub release: bool,
    pub bundle: bool,
}

/// Everything up to the APK (or AAB): config, generation, Rust, Gradle.
pub fn build(
    root: &Path,
    options: &BuildOptions,
) -> Result<(PathBuf, MobileConfig, Layout), String> {
    let config = read_config(root)?;
    if options.release && config.signing.is_none() {
        return Err(
            "A release build is signed with the app's keystore: set mobile.android.signing { keystore, alias } in reamrc.ts \
             (keytool -genkeypair -v -keystore release.keystore -alias release -keyalg RSA -keysize 2048 -validity 10000), \
             and REAM_ANDROID_STORE_PASSWORD / REAM_ANDROID_KEY_PASSWORD in the environment"
                .into(),
        );
    }
    if options.release {
        for key in ["REAM_ANDROID_STORE_PASSWORD", "REAM_ANDROID_KEY_PASSWORD"] {
            if std::env::var(key).map_or(true, |v| v.is_empty()) {
                return Err(format!(
                    "{key} is not set: the release keystore's passwords come from the environment"
                ));
            }
        }
    }
    let layout = Layout::new(root)?;
    layout.check_tooling()?;
    let natives = native_packages(&layout.root)?;
    let ndk = find_ndk(
        &|k| std::env::var(k).ok(),
        std::env::var_os("HOME").map(PathBuf::from),
    )?;
    crate::ui::info("Generating .ream/android");
    generate(&layout, &config, &natives, &ndk_version(&ndk))?;
    build_rust(&layout, &natives, &config.abis, &ndk)?;
    let task = match (options.bundle, options.release) {
        (true, _) => "bundleRelease",
        (false, true) => "assembleRelease",
        (false, false) => "assembleDebug",
    };
    crate::ui::info(&format!("Gradle {task}"));
    let gradlew = if cfg!(target_os = "windows") {
        "gradlew.bat"
    } else {
        "./gradlew"
    };
    let status = Command::new(layout.android.join(gradlew.trim_start_matches("./")))
        .current_dir(&layout.android)
        .args([
            task,
            &format!("-PreactNativeArchitectures={}", config.abis.join(",")),
        ])
        .env("ANDROID_NDK_HOME", &ndk)
        .status()
        .map_err(|e| format!("gradlew: {e}"))?;
    if !status.success() {
        return Err(format!("Gradle {task} failed"));
    }
    let outputs = layout.android.join("app/build/outputs");
    let artifact = match (options.bundle, options.release) {
        (true, _) => outputs.join("bundle/release/app-release.aab"),
        (false, true) => outputs.join("apk/release/app-release.apk"),
        (false, false) => outputs.join("apk/debug/app-debug.apk"),
    };
    if !artifact.is_file() {
        return Err(format!(
            "Gradle succeeded but {} is missing",
            artifact.display()
        ));
    }
    Ok((artifact, config, layout))
}

pub fn run_build(release: bool, bundle: bool) -> Result<(), String> {
    let root = std::env::current_dir().map_err(|e| e.to_string())?;
    let (artifact, _, _) = build(&root, &BuildOptions { release, bundle })?;
    crate::ui::success(&format!("Built {}", artifact.display()));
    Ok(())
}

fn adb() -> Command {
    let sdk_adb = ["ANDROID_HOME", "ANDROID_SDK_ROOT"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .map(|sdk| PathBuf::from(sdk).join("platform-tools/adb"))
        .find(|p| p.is_file());
    Command::new(sdk_adb.unwrap_or_else(|| PathBuf::from("adb")))
}

/// The device to install on: the one named, or the only one attached.
fn device(requested: Option<&str>) -> Result<String, String> {
    let output = adb()
        .arg("devices")
        .output()
        .map_err(|e| format!("adb: {e}"))?;
    let listed = String::from_utf8_lossy(&output.stdout);
    let devices: Vec<&str> = listed
        .lines()
        .skip(1)
        .filter_map(|l| l.split_once('\t'))
        .filter(|(_, state)| *state == "device")
        .map(|(serial, _)| serial)
        .collect();
    match requested {
        Some(serial) if devices.contains(&serial) => Ok(serial.to_string()),
        Some(serial) => Err(format!(
            "Device {serial} is not attached (adb devices: {})",
            devices.join(", ")
        )),
        None => match devices.as_slice() {
            [one] => Ok((*one).to_string()),
            [] => Err("No Android device or emulator attached (adb devices is empty)".into()),
            many => Err(format!(
                "Several devices attached ({}): pick one with --device",
                many.join(", ")
            )),
        },
    }
}

/// What adb's refusal means, and what to do about it.
fn install_failure(said: &str) -> String {
    let hint = if said.contains("INSTALL_FAILED_USER_RESTRICTED") {
        "the phone refused an install over USB: accept the prompt on its screen, or turn on \
         Developer options > Install via USB (MIUI and some others ask for each new app)"
    } else if said.contains("INSTALL_FAILED_UPDATE_INCOMPATIBLE") {
        "an app with this id is installed with another signing key: uninstall it first \
         (adb uninstall <id>), or sign with the same key"
    } else if said.contains("INSTALL_FAILED_INSUFFICIENT_STORAGE") {
        "the device is out of storage"
    } else {
        "see adb's message above"
    };
    format!("adb install failed: {hint}\n{}", said.trim())
}

pub fn run_on_device(release: bool, requested: Option<&str>) -> Result<(), String> {
    let root = std::env::current_dir().map_err(|e| e.to_string())?;
    let serial = device(requested)?;
    let (apk, config, layout) = build(
        &root,
        &BuildOptions {
            release,
            bundle: false,
        },
    )?;
    crate::ui::info(&format!("Installing on {serial}"));
    let output = adb()
        .args(["-s", &serial, "install", "-r"])
        .arg(&apk)
        .output()
        .map_err(|e| format!("adb: {e}"))?;
    if !output.status.success() {
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return Err(install_failure(&said));
    }
    // The debug build fetches its JS from Metro on the computer, and asks the
    // phone's port 8081 for it: Metro takes 8081 when it is free, else the
    // next free port — another app's Metro may hold 8081, and answering its
    // status probe, it used to be taken for ours — and adb maps 8081 to it.
    let port = if release { METRO_PORT } else { metro_port() };
    if !release {
        let _ = adb()
            .args(["-s", &serial, "reverse", "tcp:8081"])
            .arg(format!("tcp:{port}"))
            .status();
    }
    let activity = format!("{}/.MainActivity", config.id);
    let launch = || {
        adb()
            .args(["-s", &serial, "shell", "am", "start", "-n", &activity])
            .status()
            .map_err(|e| format!("adb: {e}"))
    };
    if release {
        launch()?;
        crate::ui::success(&format!("{} is running on {serial}", config.name));
        return Ok(());
    }
    // The debug build fetches its JS from Metro: Metro first, then the app.
    let mut metro = Command::new("node")
        .current_dir(&layout.root)
        .arg(layout.node_modules.join("react-native/cli.js"))
        .args(["start", "--port", &port.to_string(), "--config"])
        .arg(layout.ream.join("metro.config.cjs"))
        .spawn()
        .map_err(|e| format!("Metro: {e}"))?;
    if port != METRO_PORT {
        crate::ui::info(&format!(
            "Port {METRO_PORT} is taken: Metro serves on {port} (the phone still asks 8081)"
        ));
    }
    if !wait_for_metro(port, std::time::Duration::from_secs(60)) {
        let _ = metro.kill();
        return Err(format!(
            "Metro did not answer on localhost:{port} within a minute"
        ));
    }
    launch()?;
    crate::ui::success(&format!(
        "{} is running on {serial}; Metro serves its JS — a saved change reloads it (Ctrl+C stops Metro)",
        config.name
    ));
    let status = metro.wait().map_err(|e| format!("Metro: {e}"))?;
    if !status.success() {
        return Err("Metro stopped with an error".into());
    }
    Ok(())
}

/// The port the phone asks for its JS (React Native's default dev server port).
const METRO_PORT: u16 = 8081;

/// A free port for Metro: 8081 when nothing listens on it, else the next free one.
fn metro_port() -> u16 {
    (METRO_PORT..METRO_PORT + 100)
        .find(|port| std::net::TcpListener::bind(("127.0.0.1", *port)).is_ok())
        .unwrap_or(METRO_PORT)
}

/// Whether Metro answers its status probe on `port` before `timeout`.
fn wait_for_metro(port: u16, timeout: std::time::Duration) -> bool {
    use std::io::{Read, Write};
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", port)) {
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
            let mut reply = String::new();
            if stream
                .write_all(b"GET /status HTTP/1.0\r\nHost: localhost\r\n\r\n")
                .is_ok()
                && stream.read_to_string(&mut reply).is_ok()
                && reply.contains("packager-status:running")
            {
                return true;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    false
}

// ---------------------------------------------------------------- over-the-air updates

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The 32-byte Ed25519 seed `android:publish` signs with: REAM_UPDATES_KEY
/// (base64), else the key file.
fn signing_key(key_file: &Path) -> Result<ed25519_dalek::SigningKey, String> {
    let text = match std::env::var("REAM_UPDATES_KEY") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => std::fs::read_to_string(key_file).map_err(|e| {
            format!(
                "No update key: set REAM_UPDATES_KEY, or keep the key in {} (ream generate:update-key): {e}",
                key_file.display()
            )
        })?,
    };
    let seed: [u8; 32] = base64::engine::general_purpose::STANDARD
        .decode(text.trim())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or("The update key is not a base64 32-byte Ed25519 seed")?;
    Ok(ed25519_dalek::SigningKey::from_bytes(&seed))
}

/// `ream generate:update-key`: a new key pair for signing updates.
pub fn run_generate_update_key(out: Option<&str>, force: bool) -> Result<(), String> {
    let path = PathBuf::from(out.unwrap_or("robledo.key"));
    if path.exists() && !force {
        return Err(format!(
            "{} exists: a new key would make every installed app refuse the next update. --force replaces it",
            path.display()
        ));
    }
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| format!("No randomness from the system: {e}"))?;
    let key = ed25519_dalek::SigningKey::from_bytes(&seed);
    let encoded = base64::engine::general_purpose::STANDARD.encode(seed);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    options
        .open(&path)
        .and_then(|mut file| file.write_all(format!("{encoded}\n").as_bytes()))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let public = base64::engine::general_purpose::STANDARD.encode(key.verifying_key().to_bytes());
    crate::ui::success(&format!(
        "Wrote the update signing key to {} (readable by you only)",
        path.display()
    ));
    println!("\n  mobile.updates.publicKey: '{public}'\n");
    crate::ui::info(
        "Put the public key in reamrc.ts. Keep the private key out of git (add it to .gitignore); on CI, pass it as REAM_UPDATES_KEY.",
    );
    Ok(())
}

fn hermesc(node_modules: &Path) -> Result<PathBuf, String> {
    let os = if cfg!(target_os = "macos") {
        "osx-bin"
    } else if cfg!(target_os = "windows") {
        "win64-bin"
    } else {
        "linux64-bin"
    };
    let name = if cfg!(target_os = "windows") {
        "hermesc.exe"
    } else {
        "hermesc"
    };
    let path = node_modules
        .join("hermes-compiler/hermesc")
        .join(os)
        .join(name);
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!(
            "Hermes' compiler is missing ({}): reinstall react-native",
            path.display()
        ))
    }
}

/// Every file under `dir`, as relative forward-slash paths, sorted.
fn files_under(dir: &Path) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&at) else {
            continue;
        };
        for entry in entries {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(relative) = path.strip_prefix(dir) {
                out.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// `ream android:publish`: bundle the app's JS for its current native
/// runtime, sign it, and write it where the app's ream server (robledo)
/// serves it from. Installed apps of that runtime take it at their next start.
pub fn run_publish(key_file: Option<&str>) -> Result<(), String> {
    let root = std::env::current_dir().map_err(|e| e.to_string())?;
    let config = read_config(&root)?;
    let updates = config
        .updates
        .as_ref()
        .ok_or("mobile.updates is not set in reamrc.ts: there is nowhere to publish to")?;
    let key = signing_key(&root.join(key_file.unwrap_or("robledo.key")))?;
    let public = base64::engine::general_purpose::STANDARD.encode(key.verifying_key().to_bytes());
    if public != updates.public_key {
        return Err(
            "This key does not match mobile.updates.publicKey: installed apps would refuse what it signs".into(),
        );
    }
    let layout = Layout::new(&root)?;
    let natives = native_packages(&layout.root)?;
    let runtime = runtime_version(&layout, &natives);
    write(
        &layout.ream.join("metro.config.cjs"),
        metro_config(&layout).as_bytes(),
    )?;
    styles(&layout, &config)?;
    let work = layout.ream.join("publish");
    if work.exists() {
        std::fs::remove_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    }
    let files_dir = work.join("files");
    std::fs::create_dir_all(&files_dir).map_err(|e| e.to_string())?;
    crate::ui::info("Bundling the app's JS");
    let js = work.join("index.android.bundle.js");
    let status = Command::new("node")
        .current_dir(&layout.root)
        .arg(layout.node_modules.join("react-native/cli.js"))
        .args([
            "bundle",
            "--platform",
            "android",
            "--dev",
            "false",
            "--minify",
            "false",
            "--entry-file",
        ])
        .arg(layout.root.join(&config.entry))
        .arg("--bundle-output")
        .arg(&js)
        .arg("--assets-dest")
        .arg(&files_dir)
        .arg("--config")
        .arg(layout.ream.join("metro.config.cjs"))
        .status()
        .map_err(|e| format!("Metro: {e}"))?;
    if !status.success() {
        return Err("Bundling the JS failed".into());
    }
    crate::ui::info("Compiling it for Hermes");
    let status = Command::new(hermesc(&layout.node_modules)?)
        .args(["-emit-binary", "-O", "-out"])
        .arg(files_dir.join("index.android.bundle"))
        .arg(&js)
        .status()
        .map_err(|e| format!("hermesc: {e}"))?;
    if !status.success() {
        return Err("Hermes could not compile the bundle".into());
    }
    let mut entries = Vec::new();
    let mut bundle_digest = String::new();
    for path in files_under(&files_dir)? {
        let bytes = std::fs::read(files_dir.join(&path)).map_err(|e| format!("{path}: {e}"))?;
        let digest = hex(&Sha256::digest(&bytes));
        if path == "index.android.bundle" {
            bundle_digest = digest.clone();
        }
        entries.push(serde_json::json!({ "path": path, "sha256": digest, "size": bytes.len() }));
    }
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let id = format!("{created}-{}", bundle_digest.get(..8).unwrap_or("0"));
    let manifest = serde_json::json!({
        "id": id,
        "createdAt": created,
        "runtimeVersion": runtime,
        "platform": "android",
        "launch": "index.android.bundle",
        "files": entries,
    })
    .to_string();
    use ed25519_dalek::Signer;
    let signature =
        base64::engine::general_purpose::STANDARD.encode(key.sign(manifest.as_bytes()).to_bytes());
    let runtime_dir = layout
        .root
        .join(&updates.directory)
        .join("android")
        .join(&runtime);
    let dest = runtime_dir.join(&id);
    std::fs::create_dir_all(&runtime_dir).map_err(|e| format!("{}: {e}", runtime_dir.display()))?;
    std::fs::rename(&files_dir, dest.join("files"))
        .or_else(|_| {
            std::fs::create_dir_all(&dest)?;
            std::fs::rename(&files_dir, dest.join("files"))
        })
        .map_err(|e| format!("{}: {e}", dest.display()))?;
    write(&dest.join("manifest.json"), manifest.as_bytes())?;
    write(
        &dest.join("manifest.sig"),
        format!("{signature}\n").as_bytes(),
    )?;
    // Last: the server starts answering it only once everything is there.
    let latest_tmp = runtime_dir.join("latest.tmp");
    write(&latest_tmp, format!("{id}\n").as_bytes())?;
    std::fs::rename(&latest_tmp, runtime_dir.join("latest")).map_err(|e| format!("latest: {e}"))?;
    crate::ui::success(&format!(
        "Published update {id} for runtime {runtime} ({} files) into {}",
        files_under(&dest.join("files"))?.len(),
        dest.display()
    ));
    crate::ui::info(
        "Deploy that directory with the app: phones of this runtime take it at their next start.",
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    /// What `ream generate:update-key` and `android:publish` write and sign
    /// with, fixed: a crypto crate upgrade that changed any of it would make
    /// every installed app refuse the next update. Values from ed25519-dalek 2,
    /// sha2 0.10 and base64 0.22; the same vectors are in robledo-client.
    #[test]
    fn keeps_the_update_keys_signatures_and_digests_of_earlier_releases() {
        use base64::Engine;
        use ed25519_dalek::Signer;
        let message = b"robledo update manifest vector";
        let seed = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
        let decoded: [u8; 32] = base64::engine::general_purpose::STANDARD
            .decode(&seed)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .expect("a 32-byte seed");
        let key = ed25519_dalek::SigningKey::from_bytes(&decoded);
        assert_eq!(
            base64::engine::general_purpose::STANDARD.encode(key.verifying_key().to_bytes()),
            "6kpsY+KcUgq+9VB7Ey7F+ZVHdq6+vnuSQh7qaRRG0iw="
        );
        assert_eq!(
            base64::engine::general_purpose::STANDARD.encode(key.sign(message).to_bytes()),
            "bkEEfchf5aF/atZ+tO0U5Tb254nZNLunaeSIx5YjjzJ3iLVS7RNsDPHUsBb9acM/nIFn3H81Uc12xZl2V3MzDg=="
        );
        assert_eq!(
            hex(&Sha256::digest(message)),
            "bb558155f17e9a932b286dd54c5d5e143ff8ab009824b1a2dfdb019895e4c961"
        );
    }

    use super::*;
    use serde_json::json;

    #[test]
    fn a_package_s_start_hook_runs_before_react_native_loads() {
        let root = project("initializer");
        install(
            &root,
            "pkg",
            json!({ "reamNative": { "android": { "initializer": "com.example.Pkg.init" } } }),
        );
        let pkg = root.join("node_modules/pkg");
        let native = native_of("pkg", &pkg, &package_json(&pkg).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(native.initializer.as_deref(), Some("com.example.Pkg.init"));
        // Written into Kotlin: nothing but identifiers.
        install(
            &root,
            "bad",
            json!({ "reamNative": { "android": { "initializer": "x.y.z(); evil" } } }),
        );
        let bad = root.join("node_modules/bad");
        assert!(native_of("bad", &bad, &package_json(&bad).unwrap())
            .unwrap_err()
            .contains("is not package.Class.method"));
    }

    // A symlink, as pnpm makes it: Unix here (Windows CI compiles the tests).
    #[cfg(unix)]
    #[test]
    fn finds_react_native_s_tooling_behind_pnpm_s_link_and_names_what_is_missing() {
        let root = project("tooling");
        // pnpm's isolated linker: react-native and its own dependencies in the
        // store, only a link to react-native in the app's node_modules.
        let store = root.join("node_modules/.pnpm/react-native@0.87.1/node_modules");
        for name in [
            "react-native",
            "@react-native/gradle-plugin",
            "@react-native/codegen",
        ] {
            std::fs::create_dir_all(store.join(name)).unwrap();
            std::fs::write(
                store.join(name).join("package.json"),
                json!({ "version": "0.87.1" }).to_string(),
            )
            .unwrap();
        }
        std::os::unix::fs::symlink(
            store.join("react-native"),
            root.join("node_modules/react-native"),
        )
        .unwrap();
        let layout = Layout::new(&root).unwrap();
        assert_eq!(layout.node_modules, store.canonicalize().unwrap());
        // The app's own tooling is still missing: named at once, with the install.
        let error = layout.check_tooling().unwrap_err();
        assert!(error.contains("@react-native-community/cli"), "{error}");
        assert!(
            error.contains("@react-native/metro-config@0.87.1"),
            "{error}"
        );
        assert!(!error.contains("gradle-plugin"), "{error}");
        assert!(
            error.contains(
                "pnpm add -D @react-native-community/cli @react-native/metro-config@0.87.1"
            ),
            "{error}"
        );
        assert!(error.contains("@babel/runtime@7"), "{error}");
        install(&root, "@react-native-community/cli", json!({}));
        install(&root, "@react-native/metro-config", json!({}));
        // The 8.x line is not what React Native's Babel preset wants.
        install(&root, "@babel/runtime", json!({ "version": "8.0.0" }));
        assert!(Layout::new(&root)
            .unwrap()
            .check_tooling()
            .unwrap_err()
            .contains("@babel/runtime@7"));
        install(&root, "@babel/runtime", json!({ "version": "7.29.15" }));
        Layout::new(&root).unwrap().check_tooling().unwrap();
    }

    #[test]
    fn metro_watches_the_workspace_and_asks_esm_packages_for_their_conditions() {
        let mono = temp("workspace");
        std::fs::write(mono.join("pnpm-workspace.yaml"), "packages: [apps/*]").unwrap();
        let app = mono.join("apps/mobile");
        std::fs::create_dir_all(&app).unwrap();
        assert_eq!(workspace_root(&app), mono);
        let alone = temp("alone");
        assert_eq!(workspace_root(&alone), alone);
        let layout = Layout {
            root: app.clone(),
            ream: app.join(".ream"),
            android: app.join(".ream/android"),
            rust: app.join(".ream/rust"),
            node_modules: app.join("node_modules"),
            community_cli: None,
            workspace: mono.clone(),
        };
        let config = metro_config(&layout);
        assert!(config.contains(&serde_json::to_string(&mono.to_string_lossy()).unwrap()));
        // `import` for @c9up's ESM-only packages alone: everywhere, it gave
        // @babel/runtime's ESM helpers to CommonJS callers.
        assert!(
            config.contains(r#"unstable_conditionNames: ["react-native", "import", "default"]"#)
        );
        assert!(config.contains(r#"moduleName.startsWith("@c9up/")"#));
        assert!(config.contains(r#"nodeModulesPaths: [path.join(projectRoot, "node_modules")]"#));
        assert!(!config.contains("{{"));
    }

    #[test]
    fn metro_takes_the_next_free_port_when_8081_is_held() {
        let port = metro_port();
        let held = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
        assert_ne!(metro_port(), port);
        drop(held);
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ream-mobile-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn project(name: &str) -> PathBuf {
        let root = temp(name);
        std::fs::create_dir_all(root.join("mobile")).unwrap();
        std::fs::write(root.join("mobile/index.ts"), "").unwrap();
        root
    }

    #[test]
    fn reads_a_mobile_config_with_its_defaults() {
        let root = project("config");
        let config = parse_config(
            &json!({ "mobile": { "name": "Tessel", "id": "ch.c9up.tessel", "entry": "mobile/index.ts",
                                  "android": { "permissions": ["CAMERA", "com.example.CUSTOM"] } } }),
            &root,
        )
        .unwrap();
        assert_eq!(config.version, "1.0.0");
        assert_eq!(config.build, 1);
        assert_eq!(config.abis, vec!["arm64-v8a", "x86_64"]);
        assert_eq!(
            config.permissions,
            vec!["android.permission.CAMERA", "com.example.CUSTOM"]
        );
        assert_eq!(config.signing, None);
    }

    #[test]
    fn refuses_what_android_would() {
        let root = project("refuse");
        let base = |mobile: Value| parse_config(&json!({ "mobile": mobile }), &root).unwrap_err();
        assert!(
            base(json!({ "name": "A", "id": "tessel", "entry": "mobile/index.ts" }))
                .contains("application id")
        );
        assert!(
            base(json!({ "name": "A", "id": "ch.9up", "entry": "mobile/index.ts" }))
                .contains("application id")
        );
        assert!(
            base(json!({ "name": "A", "id": "ch.c9up.a", "entry": "nope.ts" }))
                .contains("does not exist")
        );
        assert!(base(
            json!({ "name": "A", "id": "ch.c9up.a", "entry": "mobile/index.ts", "build": 0 })
        )
        .contains("versionCode"));
        assert!(base(json!({ "name": "A", "id": "ch.c9up.a", "entry": "mobile/index.ts", "android": { "abis": ["mips"] } })).contains("mips"));
        assert!(base(json!({ "name": "A", "id": "ch.c9up.a", "entry": "mobile/index.ts", "android": { "permissions": ["A B"] } })).contains("permission"));
        assert!(parse_config(&json!({}), &root)
            .unwrap_err()
            .contains("no `mobile` block"));
    }

    #[test]
    fn styles_come_from_tailwind_or_an_empty_table_without_it() {
        let root = project("styles");
        assert!(base_css(&root, "mobile/app.css").contains("does not exist"));
        std::fs::write(root.join("mobile/app.css"), "@import \"tailwindcss\";").unwrap();
        let mut config = parse_config(
            &json!({ "mobile": { "name": "A", "id": "ch.c9up.a", "entry": "mobile/index.ts", "css": "mobile/app.css" } }),
            &root,
        )
        .unwrap();
        assert_eq!(config.css, Some(PathBuf::from("mobile/app.css")));
        install(&root, "react-native", json!({ "version": "0.87.1" }));
        let layout = Layout::new(&root).unwrap();
        // mobile.css without Tailwind installed is a mistake to name.
        assert!(styles(&layout, &config)
            .unwrap_err()
            .contains("Tailwind is not installed"));
        // Without either, the table is empty: every class reported unknown at run time.
        config.css = None;
        styles(&layout, &config).unwrap();
        let table: Value = serde_json::from_str(
            &std::fs::read_to_string(layout.ream.join("orbit-styles.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            table,
            json!({ "version": 1, "properties": {}, "rules": [], "inert": [] })
        );
    }

    fn base_css(root: &Path, css: &str) -> String {
        parse_config(
            &json!({ "mobile": { "name": "A", "id": "ch.c9up.a", "entry": "mobile/index.ts", "css": css } }),
            root,
        )
        .unwrap_err()
    }

    fn install(root: &Path, name: &str, pkg: Value) {
        let dir = root.join("node_modules").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("package.json"), pkg.to_string()).unwrap();
    }

    #[test]
    fn finds_native_packages_among_the_dependencies_direct_or_not() {
        let root = temp("natives");
        std::fs::write(
            root.join("package.json"),
            json!({ "dependencies": { "app-kit": "1" } }).to_string(),
        )
        .unwrap();
        install(
            &root,
            "app-kit",
            json!({ "dependencies": { "@c9up/orbit": "1", "plain": "1" } }),
        );
        install(&root, "plain", json!({}));
        install(
            &root,
            "@c9up/orbit",
            json!({ "reamNative": {
            "specs": "specs",
            "rust": [{ "manifest": "crates/host/Cargo.toml", "lib": "orbit_host" }],
            "android": { "java": "android/java", "cmake": { "file": "android/orbit.cmake", "link": "orbit_link" },
                         "cxxModules": [{ "name": "NativeOrbit", "header": "NativeOrbit.h" }] } } }),
        );
        let natives = native_packages(&root).unwrap();
        assert_eq!(natives.len(), 1);
        let orbit = &natives[0];
        assert_eq!(orbit.name, "@c9up/orbit");
        assert!(orbit
            .java
            .as_ref()
            .unwrap()
            .ends_with("node_modules/@c9up/orbit/android/java"));
        assert_eq!(orbit.rust[0].1, "orbit_host");
        assert_eq!(orbit.cmake.as_ref().unwrap().1, "orbit_link");
        assert_eq!(orbit.cxx_modules[0].name, "NativeOrbit");
    }

    #[test]
    fn a_native_path_cannot_leave_its_package() {
        let root = temp("escape");
        std::fs::write(
            root.join("package.json"),
            json!({ "dependencies": { "evil": "1" } }).to_string(),
        )
        .unwrap();
        install(
            &root,
            "evil",
            json!({ "reamNative": { "android": { "java": "../../../etc" } } }),
        );
        assert!(native_packages(&root)
            .unwrap_err()
            .contains("inside the package"));
        install(
            &root,
            "evil",
            json!({ "reamNative": { "android": { "cmake": { "file": "x.cmake", "link": "a(b)" } } } }),
        );
        assert!(native_packages(&root)
            .unwrap_err()
            .contains("CMake function"));
    }

    #[test]
    fn generates_the_android_project_from_the_config_and_the_natives() {
        let root = project("generate");
        let orbit = root.join("node_modules/@c9up/orbit");
        std::fs::create_dir_all(orbit.join("specs")).unwrap();
        std::fs::write(orbit.join("specs/NativeOrbit.ts"), "export {}").unwrap();
        let natives = vec![NativePackage {
            name: "@c9up/orbit".into(),
            specs: Some(orbit.join("specs")),
            rust: vec![(
                orbit.join("crates/orbit-host/Cargo.toml"),
                "orbit_host".into(),
            )],
            java: Some(orbit.join("android/src/main/java")),
            cmake: Some((orbit.join("android/orbit.cmake"), "orbit_link".into())),
            cxx_modules: vec![CxxModule {
                name: "NativeOrbit".into(),
                header: "NativeOrbit.h".into(),
            }],
            initializer: Some("com.c9up.orbit.OrbitHost.start".into()),
            ..NativePackage::default()
        }];
        let config = MobileConfig {
            name: "Tessel & co".into(),
            id: "ch.c9up.tessel".into(),
            entry: "mobile/index.ts".into(),
            version: "1.2.0".into(),
            build: 7,
            icon: None,
            permissions: vec!["android.permission.CAMERA".into()],
            abis: vec!["arm64-v8a".into()],
            signing: Some(Signing {
                keystore: "release.keystore".into(),
                alias: "release".into(),
            }),
            updates: None,
            css: None,
        };
        let layout = Layout {
            root: root.clone(),
            ream: root.join(".ream"),
            android: root.join(".ream/android"),
            rust: root.join(".ream/rust"),
            node_modules: root.join("node_modules"),
            community_cli: None,
            workspace: root.clone(),
        };
        generate(&layout, &config, &natives, "27.1.12297006").unwrap();
        let read = |p: &str| std::fs::read_to_string(layout.android.join(p)).unwrap();
        let app = read("app/build.gradle");
        assert!(app.contains("applicationId \"ch.c9up.tessel\""));
        let main = read("app/src/main/java/ch/c9up/tessel/MainApplication.kt");
        let hook = main.find("com.c9up.orbit.OrbitHost.start(this)").unwrap();
        assert!(hook < main.find("loadReactNative(this)").unwrap());
        assert!(app.contains("versionCode 7"));
        assert!(app.contains("android/src/main/java\""));
        assert!(app.contains("REAM_RUST_DIR="));
        assert!(app.contains("signingConfig signingConfigs.release"));
        assert!(app.contains("System.getenv(\"REAM_ANDROID_STORE_PASSWORD\")"));
        assert!(read("app/src/main/AndroidManifest.xml").contains("android.permission.CAMERA"));
        assert!(read("app/src/main/res/values/strings.xml").contains("Tessel &amp; co"));
        assert!(
            read("app/src/main/java/ch/c9up/tessel/MainActivity.kt").starts_with("// Generated")
        );
        let cmake = read("app/src/main/jni/CMakeLists.txt");
        assert!(cmake.contains("orbit.cmake\")\norbit_link(${CMAKE_PROJECT_NAME})"));
        let onload = read("app/src/main/jni/OnLoad.cpp");
        assert!(onload.contains("#include <NativeOrbit.h>"));
        assert!(onload.contains("std::make_shared<NativeOrbit>(jsInvoker)"));
        assert!(read("specs/NativeOrbit.ts").contains("export"));
        assert!(read("app/src/main/java/ch/c9up/tessel/MainApplication.kt")
            .contains("jsMainModulePath = \"mobile/index\""));
        assert!(read("gradle.properties").contains("reactNativeArchitectures=arm64-v8a"));
        assert!(!app.contains("{{"));
        // Unchanged inputs leave the files alone (Gradle's up-to-date checks).
        let before = std::fs::metadata(layout.android.join("app/build.gradle"))
            .unwrap()
            .modified()
            .unwrap();
        generate(&layout, &config, &natives, "27.1.12297006").unwrap();
        let after = std::fs::metadata(layout.android.join("app/build.gradle"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after);
        // A new id replaces the activity's package; the old one goes.
        let renamed = MobileConfig {
            id: "ch.c9up.other".into(),
            ..config
        };
        generate(&layout, &renamed, &natives, "27.1.12297006").unwrap();
        assert!(layout
            .android
            .join("app/src/main/java/ch/c9up/other/MainActivity.kt")
            .exists());
        assert!(!layout
            .android
            .join("app/src/main/java/ch/c9up/tessel")
            .exists());
        let config = renamed;
        // Without orbit there is nothing to render with.
        assert!(generate(&layout, &config, &[], "27")
            .unwrap_err()
            .contains("@c9up/orbit"));
    }

    #[test]
    fn explains_an_install_the_phone_refused() {
        assert!(install_failure(
            "Failure [INSTALL_FAILED_USER_RESTRICTED: Install canceled by user]"
        )
        .contains("Install via USB"));
        assert!(install_failure("INSTALL_FAILED_UPDATE_INCOMPATIBLE").contains("adb uninstall"));
    }

    /// The public key of a fixed seed.
    fn public_key() -> String {
        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        base64::engine::general_purpose::STANDARD.encode(key.verifying_key().to_bytes())
    }

    #[test]
    fn reads_the_update_server_and_refuses_an_unsafe_one() {
        let root = project("updates");
        let parse = |updates: Value| {
            parse_config(
                &json!({ "mobile": { "name": "A", "id": "ch.c9up.a", "entry": "mobile/index.ts", "updates": updates } }),
                &root,
            )
        };
        let config = parse(
            json!({ "url": "https://api.example.com/__robledo/", "publicKey": public_key() }),
        )
        .unwrap();
        let updates = config.updates.unwrap();
        assert_eq!(updates.url, "https://api.example.com/__robledo");
        assert_eq!(updates.directory, PathBuf::from("robledo"));
        assert!(parse(
            json!({ "url": "http://localhost:3333/__robledo", "publicKey": public_key() })
        )
        .is_ok());
        assert!(parse(
            json!({ "url": "http://10.0.2.2:3333/__robledo", "publicKey": public_key() })
        )
        .is_ok());
        assert!(parse(
            json!({ "url": "http://api.example.com/__robledo", "publicKey": public_key() })
        )
        .unwrap_err()
        .contains("https"));
        assert!(
            parse(json!({ "url": "http://localhost.evil.com/x", "publicKey": public_key() }))
                .is_err()
        );
        assert!(parse(json!({ "url": "https://x", "publicKey": "AAAA" }))
            .unwrap_err()
            .contains("Ed25519"));
    }

    #[test]
    fn the_runtime_version_follows_react_native_and_the_native_packages() {
        let root = temp("runtime");
        let layout = Layout {
            root: root.clone(),
            ream: root.join(".ream"),
            android: root.join(".ream/android"),
            rust: root.join(".ream/rust"),
            node_modules: root.join("node_modules"),
            community_cli: None,
            workspace: root.clone(),
        };
        install(&root, "react-native", json!({ "version": "0.87.1" }));
        let native = |name: &str, version: &str| NativePackage {
            name: name.into(),
            version: version.into(),
            ..NativePackage::default()
        };
        let a = runtime_version(
            &layout,
            &[
                native("@c9up/orbit", "0.1.0"),
                native("@c9up/robledo", "0.1.0"),
            ],
        );
        assert_eq!(a.len(), 16);
        // The order natives are found in does not matter.
        assert_eq!(
            a,
            runtime_version(
                &layout,
                &[
                    native("@c9up/robledo", "0.1.0"),
                    native("@c9up/orbit", "0.1.0")
                ]
            )
        );
        assert_ne!(
            a,
            runtime_version(
                &layout,
                &[
                    native("@c9up/orbit", "0.1.1"),
                    native("@c9up/robledo", "0.1.0")
                ]
            )
        );
        install(&root, "react-native", json!({ "version": "0.88.0" }));
        assert_ne!(
            a,
            runtime_version(
                &layout,
                &[
                    native("@c9up/orbit", "0.1.0"),
                    native("@c9up/robledo", "0.1.0")
                ]
            )
        );
    }

    #[test]
    fn wires_the_package_that_picks_the_bundle_and_writes_the_update_config() {
        let root = project("provider");
        install(&root, "react-native", json!({ "version": "0.87.1" }));
        let layout = Layout {
            root: root.clone(),
            ream: root.join(".ream"),
            android: root.join(".ream/android"),
            rust: root.join(".ream/rust"),
            node_modules: root.join("node_modules"),
            community_cli: None,
            workspace: root.clone(),
        };
        let orbit = NativePackage {
            name: "@c9up/orbit".into(),
            ..NativePackage::default()
        };
        let robledo = NativePackage {
            name: "@c9up/robledo".into(),
            version: "0.1.0".into(),
            bundle_provider: Some("com.c9up.robledo.Robledo.bundleFile".into()),
            ..NativePackage::default()
        };
        let mut config = parse_config(
            &json!({ "mobile": { "name": "A", "id": "ch.c9up.a", "entry": "mobile/index.ts",
                                 "updates": { "url": "https://x.example/__robledo", "publicKey": public_key() } } }),
            &root,
        )
        .unwrap();
        let natives = vec![orbit, robledo];
        generate(&layout, &config, &natives, "27").unwrap();
        let app = std::fs::read_to_string(
            layout
                .android
                .join("app/src/main/java/ch/c9up/a/MainApplication.kt"),
        )
        .unwrap();
        assert!(app.contains(
            "jsBundleFilePath = com.c9up.robledo.Robledo.bundleFile(applicationContext).takeUnless { BuildConfig.DEBUG }"
        ));
        let written: Value = serde_json::from_str(
            &std::fs::read_to_string(layout.android.join("app/src/main/assets/ream-updates.json"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(written["url"], "https://x.example/__robledo");
        assert_eq!(
            written["runtimeVersion"],
            runtime_version(&layout, &natives)
        );
        // https: no cleartext allowance.
        assert!(!layout
            .android
            .join("app/src/main/res/xml/ream_network_security.xml")
            .exists());
        // Updates turned off: the config goes, the app's own bundle runs.
        config.updates = None;
        generate(&layout, &config, &natives, "27").unwrap();
        assert!(!layout
            .android
            .join("app/src/main/assets/ream-updates.json")
            .exists());
        // Two packages picking the bundle: refused.
        let twice = vec![
            NativePackage {
                name: "@c9up/orbit".into(),
                ..NativePackage::default()
            },
            NativePackage {
                name: "a".into(),
                bundle_provider: Some("a.B.c".into()),
                ..NativePackage::default()
            },
            NativePackage {
                name: "b".into(),
                bundle_provider: Some("b.C.d".into()),
                ..NativePackage::default()
            },
        ];
        assert!(generate(&layout, &config, &twice, "27")
            .unwrap_err()
            .contains("keep one"));
    }

    #[test]
    fn generates_an_update_key_readable_by_its_owner_only() {
        let dir = temp("keygen");
        let path = dir.join("robledo.key");
        let path_text = path.to_string_lossy().to_string();
        run_generate_update_key(Some(&path_text), false).unwrap();
        let seed = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(seed.trim())
                .unwrap()
                .len(),
            32
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(run_generate_update_key(Some(&path_text), false)
            .unwrap_err()
            .contains("--force"));
        assert!(signing_key(&path).is_ok());
    }

    #[test]
    fn finds_the_newest_ndk_under_the_sdk() {
        let home = temp("ndk");
        for v in ["26.3.11579264", "27.1.12297006", "27.0.9"] {
            std::fs::create_dir_all(home.join("Android/Sdk/ndk").join(v)).unwrap();
        }
        let none = |_: &str| None;
        assert!(find_ndk(&none, Some(home.clone()))
            .unwrap()
            .ends_with("27.1.12297006"));
        let env_home = home
            .join("Android/Sdk/ndk/26.3.11579264")
            .to_string_lossy()
            .to_string();
        let set = move |k: &str| (k == "ANDROID_NDK_HOME").then(|| env_home.clone());
        assert!(find_ndk(&set, Some(home.clone()))
            .unwrap()
            .ends_with("26.3.11579264"));
        let missing = |k: &str| (k == "ANDROID_NDK_HOME").then(|| "/nope".to_string());
        assert!(find_ndk(&missing, None)
            .unwrap_err()
            .contains("ANDROID_NDK_HOME"));
        assert!(find_ndk(&none, Some(temp("no-ndk")))
            .unwrap_err()
            .contains("No Android NDK"));
    }
}
