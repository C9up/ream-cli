//! ream — Rust-native CLI for the Ream framework.
//!
//! Instant startup (<10ms). No Node.js boot penalty.
//! Dispatches to Node.js only for dev/start/build.

mod commands;
mod generator;
mod scaffold;
mod codemods;
mod doctor;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ream", version, about = "Ream — Rust-powered Node.js framework")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new Ream project
    New {
        /// Project name
        name: String,
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
        /// Module name (e.g., order)
        module: String,
        /// Class name (e.g., Order)
        name: String,
    },

    /// Generate an entity with decorators
    #[command(name = "make:entity")]
    MakeEntity {
        module: String,
        name: String,
    },

    /// Generate a controller with CRUD methods
    #[command(name = "make:controller")]
    MakeController {
        module: String,
        name: String,
    },

    /// Generate a validation schema
    #[command(name = "make:validator")]
    MakeValidator {
        module: String,
        name: String,
    },

    /// Generate a provider with lifecycle hooks
    #[command(name = "make:provider")]
    MakeProvider {
        name: String,
    },

    /// Generate a database migration
    #[command(name = "make:migration")]
    MakeMigration {
        name: String,
    },

    /// Configure a package (auto-setup provider, config, env)
    Configure {
        /// Package name (e.g., @c9up/atlas)
        package: String,
        /// Force overwrite existing files
        #[arg(long)]
        force: bool,
    },

    /// Run environment health checks
    Doctor,

    /// Show version info
    Info,
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::New { name } => scaffold::run(&name),
        Commands::Dev => commands::spawn_node("tsx", &["watch", "bin/server.ts"]),
        Commands::Start => commands::spawn_node("node", &["dist/bin/server.js"]),
        Commands::Build => commands::spawn_node("npx", &["tsc"]),
        Commands::MakeService { module, name } => generator::make("service", &module, &name),
        Commands::MakeEntity { module, name } => generator::make("entity", &module, &name),
        Commands::MakeController { module, name } => generator::make("controller", &module, &name),
        Commands::MakeValidator { module, name } => generator::make("validator", &module, &name),
        Commands::MakeProvider { name } => generator::make("provider", "", &name),
        Commands::MakeMigration { name } => generator::make("migration", "", &name),
        Commands::Configure { package, force } => codemods::configure(&package, force),
        Commands::Doctor => doctor::run(),
        Commands::Info => commands::info(),
    };

    if let Err(e) = result {
        eprintln!("\x1b[31merror\x1b[0m: {}", e);
        std::process::exit(1);
    }
}
