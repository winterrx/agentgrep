use anyhow::Result;
use std::path::PathBuf;
use std::time::Instant;

use crate::command::{GitCommand, ParsedCommand, parse_command};
use crate::exec::{run_shell_capture, run_shell_capture_real_tools};
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
    if trace_path.is_none() {
        return execute_run_inner(command, display_command, options);
    }

    let started = Instant::now();
    let result = execute_run_inner(command, display_command, options);
    if let (Some(path), Ok(exec_result)) = (&trace_path, &result) {
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        let _ = crate::trace::append_run_record(path, display_command, exec_result, elapsed_ms);
    }
    result
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
    let raw = run_shell_capture_real_tools(command, None)?;
    if raw_fits_budget(options, &raw.stdout, &raw.stderr) {
        return Ok(ExecResult::from_parts(
            raw.stdout,
            raw.stderr,
            raw.exit_code,
        ));
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
                return Ok(ExecResult::from_parts(
                    raw.stdout,
                    raw.stderr,
                    raw.exit_code,
                ));
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
        recovery_hint.as_deref(),
    )
}
