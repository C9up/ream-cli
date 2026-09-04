//! ream — Rust-native CLI for the Ream framework.
//!
//! Instant startup (<10ms). No Node.js boot penalty.
//! Dispatches to Node.js only for dev/start/build.

mod add;
mod codemods;
mod commands;
mod dev;
mod doctor;
mod envfile;
mod generator;
mod mcp;
mod scaffold;
mod stubs;
mod template;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "ream",
    version,
    about = "Ream — Rust-powered Node.js framework"
)]
struct Cli {
    /// Force coloured output even when stdout is not a terminal
    #[arg(long, global = true, conflicts_with = "no_ansi")]
    ansi: bool,

    /// Disable coloured output
    #[arg(long, global = true)]
    no_ansi: bool,

    /// Optional: `ream` with no command lists them, as a bare `ream` does.
    #[command(subcommand)]
    command: Option<Commands>,
}

/// Common flags shared by every `make:*` generator.
#[derive(clap::Args, Clone, Copy)]
struct GenFlags {
    /// Plan files only — emit JSON to stdout, write nothing to disk.
    #[arg(long)]
    dry_run: bool,
    /// Allow overwriting existing files.
    #[arg(long)]
    force: bool,
}

#[derive(Subcommand)]
enum McpAction {
    /// Register @c9up/ream-mcp in the project's .mcp.json
    Install,
    /// Remove the Ream MCP server from .mcp.json
    Uninstall,
    /// Show whether the Ream MCP server is registered
    Status,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new Ream project
    New {
        /// Project name
        name: String,
        /// Starting point: api, web, microservice or slim (skips the prompt)
        #[arg(long)]
        template: Option<String>,
        /// Database: postgres or sqlite (skips the prompt)
        #[arg(long)]
        db: Option<String>,
        /// Take the default for anything not passed as a flag — no terminal needed
        #[arg(long, short = 'y')]
        yes: bool,
    },

    /// Install a project template by cloning its upstream repo (e.g. kitchen-sink)
    Template {
        /// Template name (e.g. `kitchen-sink`)
        name: String,
        /// Destination directory (default: same as the template name)
        destination: Option<String>,
    },

    /// Install a package and run its configure() hook in one step
    Add {
        /// Package name (e.g., @c9up/atlas)
        package: String,
        /// Install as a devDependency
        #[arg(long)]
        dev: bool,
        /// Pass --force to the configure step (overwrites existing config files)
        #[arg(long)]
        force: bool,
        /// Unknown flags forwarded to the package's configure() hook
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        flags: Vec<String>,
    },

    /// Start development server (node --watch + swc-node; emits decorator metadata for DI)
    Dev,

    /// Start production server (spawns node)
    Start,

    /// Build TypeScript to dist/
    Build,

    /// Run the test suites declared in reamrc.ts (`tests` block)
    Test {
        /// Suite names to run. Omit to run every declared suite, in order.
        suites: Vec<String>,
        /// Concurrent worker processes
        #[arg(long)]
        threads: Option<usize>,
        /// Comma-separated reporters, e.g. spec,json
        #[arg(long)]
        reporters: Option<String>,
        /// Stop at the first failure
        #[arg(long)]
        bail: bool,
    },

    /// Generate a service class
    #[command(name = "make:service")]
    MakeService {
        module: String,
        name: String,
        #[command(flatten)]
        flags: GenFlags,
    },

    /// Generate an entity with decorators
    #[command(name = "make:entity")]
    MakeEntity {
        module: String,
        name: String,
        #[command(flatten)]
        flags: GenFlags,
    },

    /// Generate a controller with CRUD methods
    #[command(name = "make:controller")]
    MakeController {
        module: String,
        name: String,
        #[command(flatten)]
        flags: GenFlags,
    },

    /// Generate a validation schema
    #[command(name = "make:validator")]
    MakeValidator {
        module: String,
        name: String,
        #[command(flatten)]
        flags: GenFlags,
    },

    /// Generate a console command in commands/ (auto-discovered, run as `ream <name>`)
    #[command(name = "make:command")]
    MakeCommand {
        name: String,
        #[command(flatten)]
        flags: GenFlags,
    },

    /// Generate an HTTP middleware in app/middleware/
    #[command(name = "make:middleware")]
    MakeMiddleware {
        name: String,
        /// Middleware stack it is registered in: server, named, or router
        #[arg(long, default_value = "router")]
        stack: String,
        #[command(flatten)]
        flags: GenFlags,
    },

    /// Generate an event class in app/events/
    #[command(name = "make:event")]
    MakeEvent {
        name: String,
        #[command(flatten)]
        flags: GenFlags,
    },

    /// Generate an event listener in app/listeners/
    #[command(name = "make:listener")]
    MakeListener {
        name: String,
        /// Event class the listener handles (typed import + registration hint)
        #[arg(long)]
        event: Option<String>,
        #[command(flatten)]
        flags: GenFlags,
    },

    /// Generate a provider with lifecycle hooks
    #[command(name = "make:provider")]
    MakeProvider {
        name: String,
        #[command(flatten)]
        flags: GenFlags,
    },

    /// Publish a make: template into `stubs/make/` so the project can edit it
    #[command(name = "stubs:publish")]
    StubsPublish {
        /// Which stub to publish; omit to publish every one
        kind: Option<String>,
        /// List the publishable stubs and the variables each exposes
        #[arg(long)]
        list: bool,
        /// Overwrite a stub the project already published
        #[arg(long)]
        force: bool,
    },

    /// Generate a full resource module (entity + controller + validator + migration)
    #[command(name = "make:module")]
    MakeModule {
        module: String,
        name: String,
        #[command(flatten)]
        flags: GenFlags,
    },

    /// Configure a package (auto-setup provider, config, env)
    Configure {
        /// Package name (e.g., @c9up/atlas)
        package: String,
        /// Force overwrite existing files
        #[arg(long)]
        force: bool,
        /// Unknown flags forwarded to the package's configure() hook
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        flags: Vec<String>,
    },

    /// Manage the Ream MCP server registration (.mcp.json)
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },

    /// Run environment health checks
    Doctor,

    /// Inspect the registered routes, providers, and container bindings
    Inspect,

    /// Generate a fresh APP_KEY and write it into .env
    #[command(name = "generate:key")]
    GenerateKey {
        /// Replace an existing APP_KEY (invalidates cookies, sessions, signed URLs)
        #[arg(long, default_value_t = false)]
        force: bool,
        /// Print a key instead of writing it to .env
        #[arg(long, default_value_t = false)]
        show: bool,
    },

    /// Open an interactive shell with the application booted
    Repl,

    /// Show version info
    Info,

    /// List every command available here — this binary's and the app's own
    List {
        /// Only show the commands in these namespaces (`make`, `db`, …)
        #[arg(value_name = "NAMESPACE")]
        namespaces: Vec<String>,

        /// Emit JSON instead of the grouped table
        #[arg(long)]
        json: bool,
    },

    /// Any other name is dispatched to the app's console kernel.
    ///
    /// This is the app-command dispatch: `ream provision --email x`
    /// runs the app's `provision` command with its flags intact. Without it an
    /// app can only reach its own commands through a hand-written entry, which
    /// is what pushed projects to throwaway `tsx bin/*.ts` scripts.
    #[command(external_subcommand)]
    External(Vec<String>),
}

/// This binary's own commands, read back from the clap definition so `ream list`
/// cannot drift from what `ream --help` accepts.
fn framework_commands() -> Vec<commands::ListEntry> {
    use clap::CommandFactory;

    Cli::command()
        .get_subcommands()
        .filter(|sub| sub.get_name() != "help")
        .map(|sub| {
            let name = sub.get_name().to_string();
            let description = sub
                .get_about()
                .map(|about| about.to_string())
                .unwrap_or_default();
            let (args, flags) = describe_arguments(sub);
            commands::ListEntry {
                metadata: serde_json::json!({
                    "commandName": name,
                    "namespace": name.split_once(':').map(|(namespace, _)| namespace),
                    "description": description,
                    "aliases": Vec::<String>::new(),
                    "options": {},
                    "args": args,
                    "flags": flags,
                }),
                name,
                description,
            }
        })
        .collect()
}

/// A clap subcommand's arguments, in the shape the TS kernel publishes.
///
/// Without this the binary's own commands would appear in `ream list --json`
/// with no arguments at all, which reads as "this command takes none" — a
/// consumer building completions from the listing would be wrong about every
/// native command.
fn describe_arguments(sub: &clap::Command) -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
    use clap::ArgAction;

    let mut args = Vec::new();
    let mut flags = Vec::new();

    for arg in sub.get_arguments() {
        let id = arg.get_id().as_str();
        let description = arg
            .get_help()
            .map(|help| help.to_string())
            .unwrap_or_default();

        if arg.is_positional() {
            args.push(serde_json::json!({
                "type": if matches!(arg.get_action(), ArgAction::Append) { "spread" } else { "string" },
                "propertyName": id,
                "argumentName": id,
                "description": description,
                "required": arg.is_required_set(),
            }));
        } else {
            flags.push(serde_json::json!({
                "type": match arg.get_action() {
                    ArgAction::SetTrue | ArgAction::SetFalse => "boolean",
                    ArgAction::Append => "array",
                    ArgAction::Count => "number",
                    _ => "string",
                },
                "propertyName": id,
                "flagName": arg.get_long().unwrap_or(id),
                "description": description,
                "alias": arg.get_short().map(|short| short.to_string()).into_iter().collect::<Vec<_>>(),
                "required": arg.is_required_set(),
            }));
        }
    }

    (args, flags)
}

/// The name clap matched, for the commands an application could plausibly want
/// to redefine. `new` and `template` are excluded: they run outside a project,
/// where there is no app to ask.
fn native_command_name(command: &Commands) -> Option<&'static str> {
    match command {
        Commands::Dev => Some("dev"),
        Commands::Start => Some("start"),
        Commands::Build => Some("build"),
        Commands::Test { .. } => Some("test"),
        Commands::Inspect => Some("inspect"),
        Commands::Doctor => Some("doctor"),
        Commands::Info => Some("info"),
        Commands::Repl => Some("repl"),
        Commands::GenerateKey { .. } => Some("generate:key"),
        // The console kernel ships its own `list`, which is not what this means:
        // the scan only sees a `commandName` literal under `commands/`, so this
        // forwards only when the APPLICATION wrote one, and then it wins like
        // any other redefined command.
        Commands::List { .. } => Some("list"),
        _ => None,
    }
}

fn main() {
    let cli = Cli::parse();

    // The console's global colour switches. Exported so the Node side (which renders
    // most of the output) sees the same decision as this binary.
    if cli.no_ansi {
        unsafe { std::env::set_var("NO_COLOR", "1") };
        unsafe { std::env::remove_var("FORCE_COLOR") };
    } else if cli.ansi {
        unsafe { std::env::set_var("FORCE_COLOR", "1") };
        unsafe { std::env::remove_var("NO_COLOR") };
    }

    // No subcommand: a bare `ream` is `ream list`.
    let command = cli.command.unwrap_or(Commands::List {
        namespaces: Vec::new(),
        json: false,
    });

    // The console resolves every command through one registry, so an application can
    // define its own `build` or `test`. Clap matched a native command by name;
    // if the app declares that same name, the app wins and the original argv is
    // forwarded untouched.
    if let Some(name) = native_command_name(&command) {
        if commands::app_declares_command(name) {
            let argv: Vec<String> = std::env::args().skip(1).collect();
            if let Err(e) = commands::run_console(&argv) {
                eprintln!("error: {}", e);
                std::process::exit(1);
            }
            return;
        }
    }

    let result = match command {
        Commands::New {
            name,
            template,
            db,
            yes,
        } => scaffold::run(
            &name,
            &scaffold::NewOptions {
                template: template.as_deref(),
                database: db.as_deref(),
                yes,
            },
        ),
        Commands::Template { name, destination } => {
            template::run(&name, destination.as_deref())
        }
        Commands::Add { package, dev, force, flags } => add::run(&package, dev, force, &flags),
        Commands::Dev => commands::run_dev(),
        Commands::Start => commands::spawn_node("node", &["dist/bin/server.js"]),
        Commands::Build => commands::run_build(),
        Commands::Test {
            suites,
            threads,
            reporters,
            bail,
        } => commands::run_tests(&suites, threads, reporters.as_deref(), bail),
        Commands::MakeService { module, name, flags } => {
            generator::make("service", &module, &name, flags.dry_run, flags.force)
        }
        Commands::MakeEntity { module, name, flags } => {
            generator::make("entity", &module, &name, flags.dry_run, flags.force)
        }
        Commands::MakeController { module, name, flags } => {
            generator::make("controller", &module, &name, flags.dry_run, flags.force)
        }
        Commands::MakeValidator { module, name, flags } => {
            generator::make("validator", &module, &name, flags.dry_run, flags.force)
        }
        Commands::MakeCommand { name, flags } => {
            generator::make("command", "", &name, flags.dry_run, flags.force)
        }
        Commands::MakeMiddleware { name, stack, flags } => {
            generator::make_with_option("middleware", &name, Some(&stack), flags.dry_run, flags.force)
        }
        Commands::MakeEvent { name, flags } => {
            generator::make_with_option("event", &name, None, flags.dry_run, flags.force)
        }
        Commands::MakeListener { name, event, flags } => {
            generator::make_with_option(
                "listener",
                &name,
                event.as_deref(),
                flags.dry_run,
                flags.force,
            )
        }
        Commands::MakeProvider { name, flags } => {
            generator::make("provider", "", &name, flags.dry_run, flags.force)
        }
        Commands::StubsPublish { kind, list, force } => {
            if list {
                println!("Publishable stubs (written to {}/):", stubs::STUBS_DIR);
                for name in stubs::PUBLISHABLE {
                    let vars = generator::stub_variables(name).unwrap_or_default();
                    println!("  {name:<12} {{{{ {} }}}}", vars.join(" }} {{ "));
                }
                Ok(())
            } else {
                stubs::publish(kind.as_deref(), &generator::built_in_stub, force)
            }
        }
        Commands::MakeModule { module, name, flags } => {
            generator::make_module(&module, &name, flags.dry_run, flags.force)
        }
        Commands::Configure { package, force, flags } => add::parse_flag_pairs(&flags)
            .and_then(|pairs| match codemods::configure_with_flags(&package, force, &pairs)? {
                codemods::ConfigureOutcome::Configured => Ok(()),
                codemods::ConfigureOutcome::NoHook => Err(format!(
                    "Package '{0}' is not installed or does not export a configure() function.\n  \
                     Install it first: pnpm add {0}\n  \
                     The package must export {{ configure }} from its entry point or ./configure subpath.",
                    package
                )),
            }),
        Commands::Mcp { action } => match action {
            McpAction::Install => mcp::install(),
            McpAction::Uninstall => mcp::uninstall(),
            McpAction::Status => mcp::status(),
        },
        Commands::Doctor => doctor::run(),
        Commands::Inspect => commands::run_inspect(),

        Commands::GenerateKey { force, show } => commands::run_generate_key(force, show),
        Commands::Repl => commands::run_repl(),
        Commands::Info => commands::info(),
        Commands::List { namespaces, json } => {
            commands::run_list(&framework_commands(), json, &namespaces)
        }
        Commands::External(argv) => commands::run_console(&argv),
    };

    if let Err(e) = result {
        use std::io::IsTerminal;
        if std::io::stderr().is_terminal() {
            eprintln!("\x1b[31merror\x1b[0m: {}", e);
        } else {
            eprintln!("error: {}", e);
        }
        std::process::exit(1);
    }
}
