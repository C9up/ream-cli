//! ream — Rust-native CLI for the Ream framework.
//!
//! Instant startup (<10ms). No Node.js boot penalty.
//! Dispatches to Node.js only for dev/start/build.

mod add;
mod codemods;
mod commands;
mod doctor;
mod generator;
mod mcp;
mod nova;
mod scaffold;
mod template;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ream", version, about = "Ream — Rust-powered Node.js framework")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
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

    /// Start development server (spawns tsx watch)
    Dev,

    /// Start production server (spawns node)
    Start,

    /// Build TypeScript to dist/
    Build,

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

    /// Generate a provider with lifecycle hooks
    #[command(name = "make:provider")]
    MakeProvider {
        name: String,
        #[command(flatten)]
        flags: GenFlags,
    },

    /// Generate a database migration
    #[command(name = "make:migration")]
    MakeMigration {
        name: String,
        #[command(flatten)]
        flags: GenFlags,
    },

    /// Generate a database seeder
    #[command(name = "make:seeder")]
    MakeSeeder {
        module: String,
        name: String,
        #[command(flatten)]
        flags: GenFlags,
    },

    /// Generate a full resource module (entity + controller + validator + migration)
    #[command(name = "make:module")]
    MakeModule {
        module: String,
        name: String,
        #[command(flatten)]
        flags: GenFlags,
    },

    /// Run pending database migrations
    #[command(name = "migrate")]
    Migrate,

    /// Rollback the last batch of migrations
    #[command(name = "migrate:rollback")]
    MigrateRollback,

    /// Show migration status
    #[command(name = "migrate:status")]
    MigrateStatus,

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

    /// Inspect the registered routes, providers, and decorated services
    Inspect,

    /// List every registered scheduled task (cron expression, next/last run, stats)
    #[command(name = "schedule:list")]
    ScheduleList,

    /// Run a registered scheduled task once immediately (admin override — bypasses the distributed lock)
    #[command(name = "schedule:run")]
    ScheduleRun {
        /// Task name as printed by `ream schedule:list`
        name: String,
    },

    /// Generate a VAPID key pair for Web Push (writes NOVA_VAPID_* into .env)
    #[command(name = "nova:vapid:generate")]
    NovaVapidGenerate {
        /// Overwrite an existing NOVA_VAPID_PRIVATE_KEY value
        #[arg(long, default_value_t = false)]
        force: bool,
    },

    /// Show version info
    Info,
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::New { name } => scaffold::run(&name),
        Commands::Template { name, destination } => {
            template::run(&name, destination.as_deref())
        }
        Commands::Add { package, dev, force, flags } => add::run(&package, dev, force, &flags),
        Commands::Dev => commands::spawn_node("npx", &["tsx", "watch", "bin/server.ts"]),
        Commands::Start => commands::spawn_node("node", &["dist/bin/server.js"]),
        Commands::Build => commands::spawn_node("npx", &["tsc"]),
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
        Commands::MakeProvider { name, flags } => {
            generator::make("provider", "", &name, flags.dry_run, flags.force)
        }
        Commands::MakeMigration { name, flags } => {
            generator::make("migration", "", &name, flags.dry_run, flags.force)
        }
        Commands::MakeSeeder { module, name, flags } => {
            generator::make("seeder", &module, &name, flags.dry_run, flags.force)
        }
        Commands::MakeModule { module, name, flags } => {
            generator::make_module(&module, &name, flags.dry_run, flags.force)
        }
        Commands::Migrate => commands::run_migration("migrate"),
        Commands::MigrateRollback => commands::run_migration("migrate:rollback"),
        Commands::MigrateStatus => commands::run_migration("migrate:status"),
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
        Commands::ScheduleList => commands::run_schedule_list(),
        Commands::ScheduleRun { name } => commands::run_schedule_run(&name),
        Commands::NovaVapidGenerate { force } => nova::run_vapid_generate(force),
        Commands::Info => commands::info(),
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
