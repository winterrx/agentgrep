use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::bench;
use crate::doctor;
use crate::file_view;
use crate::index;
use crate::output::{ExecResult, OutputOptions};
use crate::repo_map;
use crate::run;
use crate::search;
use crate::shims;
use crate::trace;

#[derive(Debug, Parser)]
#[command(name = "agentgrep")]
#[command(
    version,
    about = "Token-efficient local command proxy for coding agents"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Proxy a familiar shell discovery command through agentgrep.
    Run(RunArgs),
    /// Search the repo with compact verified regex results.
    Regex(RegexArgs),
    /// Read or summarize a file.
    File(FileArgs),
    /// Print a compact repo map.
    Map(MapArgs),
    /// Build a lightweight local file index.
    Index(IndexArgs),
    /// Compare raw command output against proxy/indexed output.
    Bench(BenchArgs),
    /// Record, import, summarize, and replay agent command traces.
    Trace(TraceArgs),
    /// Install or inspect opt-in shell command shims.
    Shims(ShimsArgs),
    /// Execute a command from an installed shim.
    #[command(hide = true)]
    ShimExec(ShimExecArgs),
    /// Check local dependencies and agentgrep readiness.
    Doctor(DoctorArgs),
}

#[derive(Debug, Args)]
pub struct CommonOutputArgs {
    /// Emit the underlying raw output exactly where the command supports it.
    #[arg(long)]
    pub raw: bool,
    /// Emit structured JSON.
    #[arg(long)]
    pub json: bool,
    /// Treat the pattern as literal text instead of a regex where applicable.
    #[arg(long)]
    pub exact: bool,
    /// Maximum number of primary items to show.
    #[arg(long, default_value_t = 8)]
    pub limit: usize,
    /// Approximate output token budget. One token is estimated as four bytes.
    #[arg(long, default_value_t = 4000)]
    pub budget: usize,
}

impl From<&CommonOutputArgs> for OutputOptions {
    fn from(args: &CommonOutputArgs) -> Self {
        Self {
            raw: args.raw,
            json: args.json,
            exact: args.exact,
            limit: args.limit,
            budget: args.budget,
        }
    }
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Shell command to proxy, for example: agentgrep run "rg stripe".
    pub command: String,
    /// Append a trace record for this command. Use AGENTGREP_TRACE=path for global recording.
    #[arg(long)]
    pub trace: Option<PathBuf>,
    #[command(flatten)]
    pub output: CommonOutputArgs,
}

#[derive(Debug, Args)]
pub struct RegexArgs {
    /// Regex pattern to search for.
    pub pattern: String,
    /// Paths to search. Defaults to the current directory.
    #[arg(default_value = ".")]
    pub paths: Vec<PathBuf>,
    #[command(flatten)]
    pub output: CommonOutputArgs,
}

#[derive(Debug, Args)]
pub struct FileArgs {
    /// File path to read.
    pub path: PathBuf,
    /// 1-based line range, for example --lines 72:112.
    #[arg(long)]
    pub lines: Option<String>,
    #[command(flatten)]
    pub output: CommonOutputArgs,
}

#[derive(Debug, Args)]
pub struct MapArgs {
    /// Path to map. Defaults to the current directory.
    #[arg(default_value = ".")]
    pub path: PathBuf,
    #[command(flatten)]
    pub output: CommonOutputArgs,
}

#[derive(Debug, Args)]
pub struct IndexArgs {
    /// Path to index. Defaults to the current directory.
    #[arg(default_value = ".")]
    pub path: PathBuf,
    #[command(flatten)]
    pub output: CommonOutputArgs,
}

#[derive(Debug, Args)]
pub struct BenchArgs {
    /// Command to compare, for example --command 'rg stripe'.
    #[arg(long)]
    pub command: Option<String>,
    /// Replay a built-in benchmark suite, for example --suite discovery.
    #[arg(long)]
    pub suite: Option<String>,
    /// Comma-separated modes: raw,proxy,indexed.
    #[arg(long, default_value = "raw,proxy,indexed")]
    pub compare: String,
    /// Repository or fixture root to run the benchmark in.
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    /// Fail with a nonzero exit code when benchmark gates fail.
    #[arg(long)]
    pub fail_gates: bool,
    #[command(flatten)]
    pub output: CommonOutputArgs,
}

#[derive(Debug, Args)]
pub struct TraceArgs {
    #[command(subcommand)]
    pub command: TraceCommands,
}

#[derive(Debug, Args)]
pub struct ShimsArgs {
    #[command(subcommand)]
    pub command: ShimsCommands,
}

#[derive(Debug, Subcommand)]
pub enum ShimsCommands {
    /// Install POSIX shell wrappers for common agent discovery commands.
    Install(ShimsInstallArgs),
    /// Remove wrappers previously installed by agentgrep.
    Uninstall(ShimsDirArgs),
    /// Show shim install status for a directory.
    Status(ShimsDirArgs),
}

#[derive(Debug, Args)]
pub struct ShimsInstallArgs {
    /// Directory to write shims into. Put this directory before the real tools on PATH.
    #[arg(long, default_value = "~/.local/bin/agentgrep-shims")]
    pub dir: PathBuf,
    /// Agentgrep binary path to embed in wrappers. Defaults to the current executable.
    #[arg(long)]
    pub agentgrep: Option<PathBuf>,
    /// Overwrite files that are not existing agentgrep shims.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct ShimsDirArgs {
    /// Directory containing agentgrep shims.
    #[arg(long, default_value = "~/.local/bin/agentgrep-shims")]
    pub dir: PathBuf,
}

#[derive(Debug, Args)]
pub struct ShimExecArgs {
    /// Tool name the shim is standing in for.
    pub program: String,
    /// Original argv from the shim.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub enum TraceCommands {
    /// Import exec_command calls from Codex's local SQLite log.
    ImportCodex(TraceImportCodexArgs),
    /// Summarize a JSONL command trace.
    Summary(TraceSummaryArgs),
    /// Replay safe read-only commands from a trace through the benchmark harness.
    Replay(TraceReplayArgs),
}

#[derive(Debug, Args)]
pub struct TraceImportCodexArgs {
    /// Codex SQLite log path.
    #[arg(long, default_value = "~/.codex/logs_2.sqlite")]
    pub db: String,
    /// JSONL trace output path.
    #[arg(long, default_value = ".agentgrep/traces/codex.jsonl")]
    pub out: PathBuf,
    /// Only import exec calls from this working directory subtree. Defaults to the current directory.
    #[arg(long)]
    pub cwd: Option<PathBuf>,
    /// Only import a specific Codex thread id.
    #[arg(long)]
    pub thread: Option<String>,
    /// Maximum SQLite rows to scan.
    #[arg(long, default_value_t = 500)]
    pub rows: usize,
    #[command(flatten)]
    pub output: CommonOutputArgs,
}

#[derive(Debug, Args)]
pub struct TraceSummaryArgs {
    /// JSONL trace path.
    #[arg(default_value = ".agentgrep/traces/commands.jsonl")]
    pub path: PathBuf,
    #[command(flatten)]
    pub output: CommonOutputArgs,
}

#[derive(Debug, Args)]
pub struct TraceReplayArgs {
    /// JSONL trace path.
    #[arg(default_value = ".agentgrep/traces/commands.jsonl")]
    pub path: PathBuf,
    /// Repository or fixture root to replay commands in.
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    /// Comma-separated modes: raw,proxy,indexed.
    #[arg(long, default_value = "raw,proxy,indexed")]
    pub compare: String,
    /// Maximum unique safe commands to replay.
    #[arg(long, default_value_t = 20)]
    pub commands: usize,
    /// Fail with a nonzero exit code when replay gates fail.
    #[arg(long)]
    pub fail_gates: bool,
    #[command(flatten)]
    pub output: CommonOutputArgs,
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    #[command(flatten)]
    pub output: CommonOutputArgs,
}

pub fn execute(cli: Cli) -> Result<ExecResult> {
    match cli.command {
        Commands::Run(args) => {
            run::execute_run_with_trace(&args.command, (&args.output).into(), args.trace)
        }
        Commands::Regex(args) => search::execute_regex(
            &args.pattern,
            &args.paths,
            (&args.output).into(),
            Some(format!("agentgrep regex {:?}", args.pattern)),
        ),
        Commands::File(args) => {
            file_view::execute_file(&args.path, args.lines.as_deref(), (&args.output).into())
        }
        Commands::Map(args) => repo_map::execute_map(&args.path, (&args.output).into(), None),
        Commands::Index(args) => index::execute_index(&args.path, (&args.output).into()),
        Commands::Bench(args) => bench::execute_bench(args),
        Commands::Trace(args) => trace::execute_trace(args),
        Commands::Shims(args) => shims::execute_shims(args),
        Commands::ShimExec(args) => shims::execute_shim_exec(args),
        Commands::Doctor(args) => doctor::execute_doctor((&args.output).into()),
    }
}
