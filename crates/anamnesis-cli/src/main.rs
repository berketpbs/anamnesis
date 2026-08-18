//! CLI entry point for anamnesis.

use clap::Parser;

#[derive(Parser)]
#[command(name = "anamnesis")]
#[command(about = "Long-term memory for AI coding agents", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Parser)]
enum Commands {
    /// Show memory status
    Status,
    /// Search the memory wiki
    Search {
        /// Search query
        query: String,
    },
}

fn main() {
    let _cli = Cli::parse();
    println!("Anamnesis - Long-term memory for AI agents");
}
