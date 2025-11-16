mod commands;
mod config;
mod metadata;
mod runtime;
mod tasks;
mod workspace;

use clap::{Parser, Subcommand};

use commands::init::{self, InitCliArgs, InitError};

#[derive(Debug, Parser)]
#[command(
    name = "alisa",
    version,
    about = "CLI AI orchestrator",
    propagate_version = true
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Initialize the workspace (.alisa)
    Init(InitCliArgs),
}

fn main() {
    if let Err((code, message)) = run() {
        if let Some(msg) = message {
            eprintln!("{msg}");
        }
        std::process::exit(code);
    }
}

fn run() -> Result<(), (i32, Option<String>)> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init(args) => init::run(&args).map_err(|err| {
            let (code, message) = map_init_error(&err);
            (code, Some(message))
        }),
    }
}

fn map_init_error(err: &InitError) -> (i32, String) {
    match err {
        InitError::SchemaMismatch(msg) => (2, format!("Schema mismatch: {msg}")),
        InitError::WorkspaceLocked { .. } => (3, err.to_string()),
        InitError::Interrupted => (130, err.to_string()),
        InitError::ValidationFailed(_) | InitError::Other(_) => (1, err.to_string()),
    }
}
