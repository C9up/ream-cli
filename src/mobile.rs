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

use serde_json::Value;

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
    })
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

fn native_of(name: &str, root: &Path, pkg: &Value) -> Result<Option<NativePackage>, String> {
    let Some(native) = pkg.get("reamNative") else {
        return Ok(None);
    };
    let text = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).map(str::to_string);
    let mut out = NativePackage {
        name: name.to_string(),
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

/// The directories `ream android:*` writes under the project.
pub struct Layout {
    pub root: PathBuf,
    pub ream: PathBuf,
    pub android: PathBuf,
    pub rust: PathBuf,
    pub node_modules: PathBuf,
}

impl Layout {
    pub fn new(root: &Path) -> Result<Layout, String> {
        let root = root
            .canonicalize()
            .map_err(|e| format!("{}: {e}", root.display()))?;
        let rn = find_package(&root, "react-native")
            .ok_or("react-native is not installed: add it to the app's dependencies")?;
        // The node_modules holding react-native: its tooling lives beside it.
        let node_modules = rn
            .parent()
            .map(Path::to_path_buf)
            .ok_or("react-native has no parent directory")?;
        let ream = root.join(".ream");
        Ok(Layout {
            android: ream.join("android"),
            rust: ream.join("rust"),
            ream,
            root,
            node_modules,
        })
    }
}

/// Write `.ream/android` for `config` and the native packages.
pub fn generate(
    layout: &Layout,
    config: &MobileConfig,
    natives: &[NativePackage],
    ndk_version: &str,
) -> Result<(), String> {
    if !natives.iter().any(|n| n.name == "@c9up/orbit") {
        return Err("@c9up/orbit is not installed: a ream mobile app renders through it".into());
    }
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
        ("gradleName", gradle_name.clone()),
        ("minSdk", MIN_SDK.to_string()),
        ("ndkVersion", ndk_version.to_string()),
        ("abis", config.abis.join(",")),
        ("version", config.version.replace('"', "")),
        ("id", config.id.clone()),
        ("build", config.build.to_string()),
        ("entry", gradle_path(&layout.root.join(&config.entry))),
        ("mainModule", main_module(&config.entry)),
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
    write(&layout.ream.join("metro.config.cjs"), T_METRO.as_bytes())?;
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
    if !release {
        // The debug build fetches its JS from Metro on the computer.
        let _ = adb()
            .args(["-s", &serial, "reverse", "tcp:8081", "tcp:8081"])
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
        .args(["start", "--config"])
        .arg(layout.ream.join("metro.config.cjs"))
        .spawn()
        .map_err(|e| format!("Metro: {e}"))?;
    if !wait_for_metro(std::time::Duration::from_secs(60)) {
        let _ = metro.kill();
        return Err("Metro did not answer on localhost:8081 within a minute".into());
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

/// Whether Metro answers its status probe before `timeout`.
fn wait_for_metro(timeout: std::time::Duration) -> bool {
    use std::io::{Read, Write};
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", 8081)) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        };
        let layout = Layout {
            root: root.clone(),
            ream: root.join(".ream"),
            android: root.join(".ream/android"),
            rust: root.join(".ream/rust"),
            node_modules: root.join("node_modules"),
        };
        generate(&layout, &config, &natives, "27.1.12297006").unwrap();
        let read = |p: &str| std::fs::read_to_string(layout.android.join(p)).unwrap();
        let app = read("app/build.gradle");
        assert!(app.contains("applicationId \"ch.c9up.tessel\""));
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
