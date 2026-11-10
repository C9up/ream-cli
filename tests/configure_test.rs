//! Integration tests for the configure() hook dispatch (`ream configure`,
//! `ream add`).
//!
//! Real `node`, fixture packages in plain JS: a no-op TypeScript loader, and a
//! `@c9up/ream` stand-in whose codemods record each call and whose `Prompt`
//! fails if it is ever reached — a test process has no terminal, so a prompt
//! must be answered by its default, or refused, before getting there.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use wait_timeout::ChildExt;

struct TempProject {
    path: PathBuf,
}

impl TempProject {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ream-configure-test-{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&path).expect("create temp project dir");
        let p = Self { path };
        p.write(
            "package.json",
            r#"{ "name": "fixture", "private": true, "type": "module" }"#,
        );
        p.write(
            "node_modules/@swc-node/register/package.json",
            r#"{ "name": "@swc-node/register", "type": "module",
  "exports": { "./esm-register": "./esm-register.js" } }"#,
        );
        p.write("node_modules/@swc-node/register/esm-register.js", "");
        p.write(
            "node_modules/@c9up/ream/package.json",
            r#"{ "name": "@c9up/ream", "type": "module", "exports": { ".": "./index.js" } }"#,
        );
        p.write(
            "node_modules/@c9up/ream/index.js",
            r#"import { appendFileSync } from 'node:fs'
const record = (entry) => appendFileSync('calls.log', JSON.stringify(entry) + '\n')
export function createCodemods(options = {}) {
  return {
    async addProvider(path) { record({ addProvider: path, force: options.force === true }) },
    async addEnvVars(vars) { record({ addEnvVars: vars }) },
  }
}
const reached = async (message) => { throw new Error(`the interactive prompt was reached for "${message}"`) }
export class Prompt {
  ask(m) { return reached(m) }
  secure(m) { return reached(m) }
  confirm(m) { return reached(m) }
  toggle(m) { return reached(m) }
  choice(m) { return reached(m) }
  multiple(m) { return reached(m) }
  autocomplete(m) { return reached(m) }
}
export class Ui {
  constructor() {
    this.colors = { green: (text) => text }
    this.logger = {
      success: (m) => console.log(`[ success ] ${m}`),
      error: (m) => console.error(`[ error ] ${m}`),
      warning: (m) => console.error(`[ warn ] ${m}`),
      info: (m) => console.log(`[ info ] ${m}`),
    }
  }
}
"#,
        );
        p
    }

    fn write(&self, rel: &str, content: &str) {
        let abs = self.path.join(rel);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(&abs, content).expect("write file");
    }

    /// A package whose root module is `source`.
    fn package(&self, name: &str, source: &str) {
        self.write(
            &format!("node_modules/{name}/package.json"),
            &format!(
                r#"{{ "name": "{name}", "type": "module", "exports": {{ ".": "./index.js" }} }}"#
            ),
        );
        self.write(&format!("node_modules/{name}/index.js"), source);
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.path.join(rel)).unwrap_or_default()
    }

    /// `ream <args>` with no terminal on stdin, killed after 30 s so a prompt
    /// that waits for input fails the test instead of hanging it.
    fn ream(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ream"));
        command
            .args(args)
            .current_dir(&self.path)
            .env_remove("CI")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("spawn ream");
        match child.wait_timeout(Duration::from_secs(30)).expect("wait") {
            Some(_) => child.wait_with_output().expect("collect output"),
            None => {
                let _ = child.kill();
                let output = child.wait_with_output().expect("collect output");
                panic!(
                    "ream {args:?} did not finish: a prompt waited for input\n{}",
                    text(&output)
                );
            }
        }
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn text(output: &Output) -> String {
    format!(
        "status: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn a_hook_written_against_codemods_and_flags_still_runs() {
    let p = TempProject::new("legacy");
    p.package(
        "legacy-pkg",
        r#"import { writeFileSync } from 'node:fs'
export async function configure(codemods, flags) {
  await codemods.addProvider('legacy-pkg/provider')
  writeFileSync('flags.json', JSON.stringify(flags))
}"#,
    );

    let output = p.ream(
        &["configure", "legacy-pkg", "--force", "--driver=pg", "--ssl"],
        &[],
    );
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(
        p.read("calls.log").trim(),
        r#"{"addProvider":"legacy-pkg/provider","force":true}"#
    );
    assert_eq!(p.read("flags.json"), r#"{"driver":["pg"],"ssl":["true"]}"#);
}

#[test]
fn the_hook_gets_a_command_with_parsed_flags_and_codemods() {
    let p = TempProject::new("command");
    p.package(
        "modern-pkg",
        r#"import { writeFileSync } from 'node:fs'
export const stubsRoot = '/stubs/of/modern-pkg'
export async function configure(command) {
  const codemods = await command.createCodemods()
  await codemods.addEnvVars({ LOCK_STORE: command.parsedFlags.store })
  writeFileSync('command.json', JSON.stringify({
    name: command.name,
    force: command.force,
    parsedFlags: command.parsedFlags,
    parsedArgs: command.parsedArgs,
    stubsRoot: command.stubsRoot,
    logger: typeof command.logger.success,
    prompt: typeof command.prompt.choice,
  }))
}"#,
    );

    let output = p.ream(
        &[
            "configure",
            "modern-pkg",
            "--store=redis",
            "--retries=3",
            "--tag",
            "a",
            "--tag",
            "b",
            "--no-color",
        ],
        &[],
    );
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(
        p.read("command.json"),
        r#"{"name":"modern-pkg","force":false,"parsedFlags":{"store":"redis","retries":3,"tag":["a","b"],"color":false},"parsedArgs":[],"stubsRoot":"/stubs/of/modern-pkg","logger":"function","prompt":"function"}"#
    );
    assert_eq!(
        p.read("calls.log").trim(),
        r#"{"addEnvVars":{"LOCK_STORE":"redis"}}"#
    );
}

/// The shape of `@adonisjs/lock`'s hook: the flag answers, else the prompt asks.
const LOCK_LIKE: &str = r#"import { writeFileSync } from 'node:fs'
export async function configure(command) {
  let store = command.parsedFlags.store
  if (!store) {
    store = await command.prompt.choice('Select the storage layer', ['redis', 'memory'], {
      default: process.env.STORE_DEFAULT === undefined ? undefined : Number(process.env.STORE_DEFAULT),
    })
  }
  if (!['redis', 'memory'].includes(store)) {
    command.exitCode = 1
    command.logger.error(`Invalid lock store "${store}"`)
    return
  }
  const codemods = await command.createCodemods()
  await codemods.addEnvVars({ LOCK_STORE: store })
}"#;

#[test]
fn a_prompt_without_a_terminal_or_a_default_fails_at_once_and_says_why() {
    let p = TempProject::new("no-default");
    p.package("lock-like", LOCK_LIKE);

    let output = p.ream(&["configure", "lock-like"], &[]);
    assert!(!output.status.success(), "{}", text(&output));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("E_CONSOLE_NOT_INTERACTIVE")
            && stderr.contains("Select the storage layer")
            && stderr.contains("ream configure lock-like"),
        "{}",
        text(&output)
    );
    assert!(
        !stderr.contains("interactive prompt was reached"),
        "{}",
        text(&output)
    );
    assert_eq!(p.read("calls.log"), "", "nothing may be written");
}

#[test]
fn a_prompt_without_a_terminal_takes_its_default() {
    let p = TempProject::new("default");
    p.package("lock-like", LOCK_LIKE);

    let output = p.ream(&["configure", "lock-like"], &[("STORE_DEFAULT", "1")]);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(
        p.read("calls.log").trim(),
        r#"{"addEnvVars":{"LOCK_STORE":"memory"}}"#
    );
}

#[test]
fn under_ci_a_prompt_is_not_waited_on_either() {
    let p = TempProject::new("ci");
    p.package("lock-like", LOCK_LIKE);

    let output = p.ream(&["configure", "lock-like"], &[("CI", "true")]);
    assert!(!output.status.success(), "{}", text(&output));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not interactive (CI)"), "{}", text(&output));
}

#[test]
fn a_flag_answers_the_question_without_prompting() {
    let p = TempProject::new("flag-answer");
    p.package("lock-like", LOCK_LIKE);

    let output = p.ream(&["configure", "lock-like", "--store=redis"], &[]);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(
        p.read("calls.log").trim(),
        r#"{"addEnvVars":{"LOCK_STORE":"redis"}}"#
    );
}

#[test]
fn an_exit_code_set_by_the_hook_fails_the_command() {
    let p = TempProject::new("exit-code");
    p.package("lock-like", LOCK_LIKE);

    let output = p.ream(&["configure", "lock-like", "--store=dynamo"], &[]);
    assert!(!output.status.success(), "{}", text(&output));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Invalid lock store \"dynamo\""),
        "{}",
        text(&output)
    );
    assert_eq!(p.read("calls.log"), "");
}

#[test]
fn every_prompt_kind_answers_with_its_default_and_runs_validate_and_result() {
    let p = TempProject::new("all-kinds");
    p.package(
        "asker",
        r#"import { writeFileSync } from 'node:fs'
export async function configure(command) {
  const answers = {
    ask: await command.prompt.ask('Name', { default: 'app', result: (v) => v.toUpperCase() }),
    secure: await command.prompt.secure('Token', { default: 't0k' }),
    confirm: await command.prompt.confirm('Sure?', { default: true }),
    toggle: await command.prompt.toggle('Mode', ['on', 'off'], { default: false }),
    choice: await command.prompt.choice('Pick', ['a', { name: 'b', message: 'B' }], { default: 1 }),
    multiple: await command.prompt.multiple('Several', ['x', 'y', 'z'], { default: [0, 2] }),
    autocomplete: await command.prompt.autocomplete('Find', ['p', 'q'], { default: 0 }),
  }
  writeFileSync('answers.json', JSON.stringify(answers))
  await command.prompt.ask('Port', { default: 'abc', validate: (v) => /^\d+$/.test(v) || 'not a number' })
}"#,
    );

    let output = p.ream(&["configure", "asker"], &[]);
    assert_eq!(
        p.read("answers.json"),
        r#"{"ask":"APP","secure":"t0k","confirm":true,"toggle":false,"choice":"b","multiple":["x","z"],"autocomplete":"p"}"#,
        "{}",
        text(&output)
    );
    // A default its own validation rejects is not silently accepted.
    assert!(!output.status.success(), "{}", text(&output));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not a number"), "{}", text(&output));
}

#[test]
fn ream_add_hands_the_same_command_to_the_hook() {
    let p = TempProject::new("add");
    p.write("pnpm-lock.yaml", "");
    p.package("lock-like", LOCK_LIKE);
    let log = p.path.join("dry-run.log");

    let output = p.ream(
        &["add", "lock-like", "--store", "memory"],
        &[
            ("REAM_ADD_DRY_RUN", "1"),
            ("REAM_ADD_DRY_RUN_LOG", &log.to_string_lossy()),
        ],
    );
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(
        p.read("calls.log").trim(),
        r#"{"addEnvVars":{"LOCK_STORE":"memory"}}"#
    );
}
