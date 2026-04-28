pub mod bench;
pub mod cli;
pub mod command;
pub mod deps;
pub mod doctor;
pub mod exec;
pub mod file_view;
pub mod filters;
pub mod git_compact;
pub mod index;
pub mod line_read;
pub mod output;
pub mod parser;
pub mod repo_map;
pub mod run;
pub mod search;
pub mod shims;
pub mod tee;
pub mod test_runner;
pub mod trace;
pub mod tracking;

use anyhow::Result;
use clap::Parser;

pub use output::ExecResult;

pub fn run_cli() -> Result<ExecResult> {
    let cli = cli::Cli::parse();
    cli::execute(cli)
}
