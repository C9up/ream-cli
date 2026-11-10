//! Finding the external programs this binary starts.
//!
//! Two rules, both from the way upstream starts its tools (execa with
//! `preferLocal`):
//!
//! - A project tool (`tsc`) is the project's own: `node_modules/.bin` of the
//!   project and of every directory above it (a workspace hoists it to the
//!   root), then `PATH`. Never `npx <name>`: when nothing local answers, npx
//!   treats the name as a package to download, and without a terminal it does
//!   so without asking — `npx tsc` installs the deprecated `tsc@2.0.4`.
//! - On Windows, `pnpm`, `npm`, `npx` and `yarn` are `.cmd` shims. Rust's
//!   `Command` only appends `.exe` to a bare name, so they are looked up here
//!   through `PATHEXT` and started by their full path.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The extensions a bare name may carry on this platform, in lookup order.
///
/// Unix runs the name as it is. Windows reads `PATHEXT`, keeping only what
/// `CreateProcess` (through Rust's batch-file handling) can start: an
/// extension such as `.JS` or `.VBS` in that list belongs to a script host.
fn executable_extensions() -> Vec<String> {
    if cfg!(windows) {
        windows_extensions(std::env::var("PATHEXT").ok().as_deref())
    } else {
        Vec::new()
    }
}

/// `PATHEXT`, filtered to what can be spawned, with the system default when unset.
fn windows_extensions(pathext: Option<&str>) -> Vec<String> {
    let raw = pathext
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(".EXE;.CMD;.BAT");
    raw.split(';')
        .map(|ext| ext.trim().to_ascii_lowercase())
        .filter(|ext| matches!(ext.as_str(), ".exe" | ".cmd" | ".bat" | ".com"))
        .collect()
}

/// Is `path` a file this process could execute?
fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// The first `dir/name[ext]` that exists and is executable.
///
/// With no extensions the name is tried as written; with some, each is tried
/// in order and the bare name is not (a Windows `pnpm` without extension is
/// the POSIX shell script npm also installs, which `CreateProcess` cannot run).
pub(crate) fn find_in_dirs<I>(name: &str, dirs: I, extensions: &[String]) -> Option<PathBuf>
where
    I: IntoIterator<Item = PathBuf>,
{
    for dir in dirs {
        if extensions.is_empty() {
            let candidate = dir.join(name);
            if is_executable(&candidate) {
                return Some(candidate);
            }
            continue;
        }
        for ext in extensions {
            let candidate = dir.join(format!("{name}{ext}"));
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// `name` looked up on `PATH` with this platform's extensions.
fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    find_in_dirs(name, std::env::split_paths(&path), &executable_extensions())
}

/// A `Command` for a program found on `PATH` — `pnpm`, `npm`, `yarn`, `git`.
///
/// On Unix the name is handed to `Command` untouched, which searches `PATH`
/// itself. On Windows it is resolved here first so a `.cmd` shim is found; a
/// name that resolves to nothing is passed through so the spawn error still
/// says "program not found" for the name the user knows.
pub fn command(name: &str) -> Command {
    if cfg!(windows) {
        if let Some(found) = on_path(name) {
            return Command::new(found);
        }
    }
    Command::new(name)
}

/// The `node_modules/.bin` directories from `start` up to the filesystem root,
/// nearest first — the order execa's `preferLocal` puts them on `PATH`.
fn local_bin_dirs(start: &Path) -> impl Iterator<Item = PathBuf> + '_ {
    start
        .ancestors()
        .map(|dir| dir.join("node_modules").join(".bin"))
}

/// A project-local bin, nearest `node_modules/.bin` first.
pub fn local_bin(start: &Path, name: &str) -> Option<PathBuf> {
    find_in_dirs(name, local_bin_dirs(start), &executable_extensions())
}

/// A `Command` for the project's TypeScript compiler.
///
/// The project's own `tsc`, else one on `PATH`. Nothing else: no registry
/// download under a guessed name.
pub fn tsc(root: &Path) -> Result<Command, String> {
    let found = local_bin(root, "tsc").or_else(|| on_path("tsc"));
    match found {
        Some(path) => Ok(Command::new(path)),
        None => Err(
            "TypeScript is not installed in this project, so there is no `tsc` to build with.\n  \
             Run `pnpm add -D typescript` (or your package manager's equivalent)."
                .to_string(),
        ),
    }
}

/// The program a `Command` starts, for an error message.
pub fn program_name(command: &Command) -> String {
    Path::new(command.get_program())
        .file_name()
        .unwrap_or_else(|| OsStr::new("?"))
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ream-tools-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn executable(path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn exts(list: &[&str]) -> Vec<String> {
        list.iter().map(|ext| (*ext).to_string()).collect()
    }

    #[test]
    fn a_cmd_shim_is_found_when_no_exe_exists() {
        // npm, npx, yarn and a corepack pnpm on Windows: only the shim exists.
        let dir = tempdir("cmd-shim");
        executable(&dir.join("pnpm.cmd"));
        let found = find_in_dirs("pnpm", [dir.clone()], &exts(&[".exe", ".cmd"]));
        assert_eq!(found, Some(dir.join("pnpm.cmd")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_exe_wins_over_a_cmd_in_the_same_directory() {
        let dir = tempdir("exe-first");
        executable(&dir.join("pnpm.cmd"));
        executable(&dir.join("pnpm.exe"));
        let found = find_in_dirs("pnpm", [dir.clone()], &exts(&[".exe", ".cmd"]));
        assert_eq!(found, Some(dir.join("pnpm.exe")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_extensionless_posix_script_is_not_taken_on_windows() {
        // npm installs `pnpm` (a sh script) beside `pnpm.cmd`; CreateProcess
        // cannot run the former.
        let dir = tempdir("no-bare");
        executable(&dir.join("pnpm"));
        assert_eq!(
            find_in_dirs("pnpm", [dir.clone()], &exts(&[".exe", ".cmd"])),
            None
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pathext_keeps_only_what_can_be_spawned() {
        assert_eq!(
            windows_extensions(Some(".COM;.EXE;.BAT;.CMD;.VBS;.JS;.WSH")),
            exts(&[".com", ".exe", ".bat", ".cmd"])
        );
        assert_eq!(windows_extensions(None), exts(&[".exe", ".cmd", ".bat"]));
        assert_eq!(
            windows_extensions(Some("  ")),
            exts(&[".exe", ".cmd", ".bat"])
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_non_executable_file_is_not_a_program() {
        let dir = tempdir("not-exec");
        std::fs::write(dir.join("tsc"), "").unwrap();
        assert_eq!(find_in_dirs("tsc", [dir.clone()], &[]), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_nearest_local_bin_wins_and_a_workspace_root_is_reached() {
        let root = tempdir("local-bin");
        let app = root.join("apps/api");
        std::fs::create_dir_all(&app).unwrap();
        let ext = if cfg!(windows) { ".cmd" } else { "" };
        let hoisted = root.join(format!("node_modules/.bin/tsc{ext}"));
        executable(&hoisted);
        assert_eq!(local_bin(&app, "tsc"), Some(hoisted));

        let own = app.join(format!("node_modules/.bin/tsc{ext}"));
        executable(&own);
        assert_eq!(local_bin(&app, "tsc"), Some(own));
        let _ = std::fs::remove_dir_all(root);
    }
}
