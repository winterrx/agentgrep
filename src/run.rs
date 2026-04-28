use anyhow::Result;
use std::path::PathBuf;
use std::time::Instant;

use crate::command::{GitCommand, ParsedCommand, SearchKind, parse_command};
use crate::exec::{
    run_shell_capture, run_shell_capture_optimized_real_tools, run_shell_capture_real_tools,
};
use crate::file_view;
use crate::git_compact;
use crate::line_read;
use crate::output::{ExecResult, OutputOptions, raw_fits_budget};
use crate::repo_map;
use crate::search::{self, summary_from_matches};

pub fn execute_run(command: &str, options: OutputOptions) -> Result<ExecResult> {
    execute_run_with_trace(command, options, None)
}

pub fn execute_run_with_trace(
    command: &str,
    options: OutputOptions,
    trace_path: Option<PathBuf>,
) -> Result<ExecResult> {
    execute_run_with_trace_label(command, command, options, trace_path)
}

pub fn execute_run_with_trace_label(
    command: &str,
    display_command: &str,
    options: OutputOptions,
    trace_path: Option<PathBuf>,
) -> Result<ExecResult> {
    let trace_path = crate::trace::resolve_trace_path(trace_path);

    let started = Instant::now();
    let result = execute_run_inner(command, display_command, options);
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    if let (Some(path), Ok(exec_result)) = (&trace_path, &result) {
        let _ = crate::trace::append_run_record(path, display_command, exec_result, elapsed_ms);
    }
    if let Ok(exec_result) = &result {
        let _ = append_tracking_record(display_command, exec_result, elapsed_ms);
    }
    result
}

fn append_tracking_record(command: &str, exec_result: &ExecResult, elapsed_ms: f64) -> Result<()> {
    let output_tokens = crate::output::estimate_tokens_from_bytes(
        exec_result.stdout.len() + exec_result.stderr.len(),
    );
    let baseline = exec_result.baseline_output_tokens.unwrap_or(output_tokens);
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let record = crate::tracking::TrackingRecord::from_input(crate::tracking::TrackingInput {
        command: command.to_string(),
        optimized_command_label: "agentgrep run".to_string(),
        cwd,
        project: None,
        input_tokens: baseline as u64,
        output_tokens: output_tokens as u64,
        baseline_output_tokens: Some(baseline as u64),
        elapsed_ms: elapsed_ms.round() as u64,
    });
    crate::tracking::append_tracking_record(&record)
}

fn execute_run_inner(
    command: &str,
    display_command: &str,
    options: OutputOptions,
) -> Result<ExecResult> {
    let options = options.normalized();
    if options.raw || std::env::var("AGENTGREP_DISABLE").ok().as_deref() == Some("1") {
        return passthrough_real_tools(command);
    }

    let parsed = match parse_command(command) {
        Ok(parsed) => parsed,
        Err(_) => return passthrough(command),
    };
    match parsed {
        ParsedCommand::Search(search_command) => {
            execute_search_proxy(command, display_command, search_command, options)
        }
        ParsedCommand::FindMap { query } => {
            if !query.path.exists() {
                return passthrough_real_tools(command);
            }
            repo_map::execute_find_map(&query, options, Some(display_command.to_string()))
        }
        ParsedCommand::LsRecursive { path } => {
            if !path.exists() {
                return passthrough_real_tools(command);
            }
            repo_map::execute_map(&path, options, Some(display_command.to_string()))
        }
        ParsedCommand::TreeMap { path } => {
            if !path.exists() {
                return passthrough_real_tools(command);
            }
            repo_map::execute_map(&path, options, Some(display_command.to_string()))
        }
        ParsedCommand::Cat { path } => {
            if !path.exists() {
                return passthrough_real_tools(command);
            }
            file_view::execute_file(&path, None, options)
        }
        ParsedCommand::FileSlice(slice) => {
            if !slice.path.exists() {
                return passthrough_real_tools(command);
            }
            line_read::execute_file_slice(command, slice, options)
        }
        ParsedCommand::WcLines { paths } => line_read::execute_wc_lines(command, paths, options),
        ParsedCommand::Git(GitCommand::ReadOnly { subcommand, .. }) => {
            git_compact::execute_git(command, subcommand, options)
        }
        ParsedCommand::Git(GitCommand::Mutating { .. }) => passthrough_real_tools(command),
        ParsedCommand::Test(runner) => crate::test_runner::execute_test(command, runner, options),
        ParsedCommand::Deps { path } => crate::deps::execute_deps(&path, options),
        ParsedCommand::Unsupported { .. } if is_shimmed_command_family(command) => {
            passthrough_real_tools(command)
        }
        ParsedCommand::Unsupported { .. } => passthrough(command),
    }
}

fn is_shimmed_command_family(command: &str) -> bool {
    let Ok(words) = shell_words::split(command) else {
        return false;
    };
    let Some(executable) = words.first() else {
        return false;
    };
    let name = std::path::Path::new(executable)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(executable);
    matches!(
        name,
        "rg" | "grep"
            | "find"
            | "ls"
            | "cat"
            | "git"
            | "head"
            | "tail"
            | "sed"
            | "nl"
            | "wc"
            | "tree"
            | "cargo"
            | "pytest"
            | "py.test"
            | "python"
            | "python3"
            | "go"
            | "npm"
            | "pnpm"
            | "yarn"
            | "npx"
            | "vitest"
            | "jest"
            | "playwright"
            | "ruff"
            | "mypy"
            | "deps"
    )
}

pub fn passthrough(command: &str) -> Result<ExecResult> {
    let captured = run_shell_capture(command, None)?;
    Ok(ExecResult::from_parts(
        captured.stdout,
        captured.stderr,
        captured.exit_code,
    ))
}

pub fn passthrough_real_tools(command: &str) -> Result<ExecResult> {
    let captured = run_shell_capture_real_tools(command, None)?;
    Ok(ExecResult::from_parts(
        captured.stdout,
        captured.stderr,
        captured.exit_code,
    ))
}

fn execute_search_proxy(
    command: &str,
    display_command: &str,
    search_command: crate::command::SearchCommand,
    options: OutputOptions,
) -> Result<ExecResult> {
    if let Some(result) = try_execute_plain_rg_fast_path(display_command, &search_command, options)?
    {
        return Ok(result);
    }

    let raw = run_shell_capture_optimized_real_tools(command, None)?;
    let raw_tokens = raw.output_tokens();
    if !raw.stdout_truncated && raw_fits_budget(options, &raw.stdout, &raw.stderr) {
        return Ok(
            ExecResult::from_parts(raw.stdout, raw.stderr, raw.exit_code)
                .with_baseline_output_tokens(raw_tokens),
        );
    }
    let limit = options.limit;

    let summary = if search_command.prefer_raw_matches {
        match search::summary_from_raw_match_lines(
            &search_command.pattern,
            &search_command.paths,
            &raw.stdout,
            limit,
        ) {
            Some(summary) => summary,
            None => {
                if raw.stdout_truncated {
                    summary_from_matches(
                        &search_command.pattern,
                        &search_command.paths,
                        0,
                        1,
                        raw.stdout_bytes,
                        Vec::new(),
                    )
                } else {
                    return Ok(ExecResult::from_parts(
                        raw.stdout,
                        raw.stderr,
                        raw.exit_code,
                    ));
                }
            }
        }
    } else {
        match search::search_paths(
            &search_command.pattern,
            &search_command.paths,
            options.exact,
            limit,
        ) {
            Ok(summary) => summary,
            Err(_) => {
                let parsed = search::parse_raw_match_lines(&raw.stdout, limit);
                let total = String::from_utf8_lossy(&raw.stdout).lines().count();
                summary_from_matches(
                    &search_command.pattern,
                    &search_command.paths,
                    0,
                    total,
                    raw.stdout_bytes,
                    parsed,
                )
            }
        }
    };

    let recovery_hint = crate::tee::tee_raw_output(
        display_command,
        &raw.stdout,
        &raw.stderr,
        summary.truncated || raw.exit_code != 0,
    );
    search::render_search_result(
        &summary,
        options,
        display_command,
        raw.exit_code,
        &raw.stderr,
        raw.capture_hint(recovery_hint.as_deref()).as_deref(),
    )
    .map(|result| result.with_baseline_output_tokens(raw_tokens))
}

fn try_execute_plain_rg_fast_path(
    display_command: &str,
    search_command: &crate::command::SearchCommand,
    options: OutputOptions,
) -> Result<Option<ExecResult>> {
    if search_command.kind != SearchKind::Rg || search_command.prefer_raw_matches {
        return Ok(None);
    }
    if search_command.paths.iter().any(|path| !path.exists()) {
        return Ok(None);
    }

    let summary = match search::search_paths(
        &search_command.pattern,
        &search_command.paths,
        options.exact,
        options.limit,
    ) {
        Ok(summary) => summary,
        Err(_) => return Ok(None),
    };
    let raw_tokens = crate::output::estimate_tokens_from_bytes(summary.raw_output_bytes);
    if raw_tokens <= options.budget {
        return Ok(None);
    }
    let exit_code = if summary.total_matches == 0 { 1 } else { 0 };
    search::render_search_result(&summary, options, display_command, exit_code, &[], None)
        .map(|result| Some(result.with_baseline_output_tokens(raw_tokens)))
}
