use anyhow::Result;
use serde::Serialize;

use crate::command::GitReadOnly;
use crate::exec::CapturedCommand;
use crate::output::{
    ExecResult, OutputOptions, json_result, push_budgeted_line, raw_fits_budget, status_footer,
};

#[derive(Debug, Clone, Serialize)]
pub struct GitSummary {
    pub subcommand: String,
    pub raw_lines: usize,
    pub shown_lines: usize,
    pub omitted_lines: usize,
    pub truncated: bool,
    pub lines: Vec<String>,
}

pub fn execute_git(
    command: &str,
    subcommand: GitReadOnly,
    options: OutputOptions,
) -> Result<ExecResult> {
    let options = options.normalized();
    if options.raw {
        let captured = crate::exec::run_shell_capture_real_tools(command, None)?;
        return Ok(ExecResult::from_parts(
            captured.stdout,
            captured.stderr,
            captured.exit_code,
        ));
    }

    let captured = crate::exec::run_shell_capture_real_tools(command, None)?;
    if raw_fits_budget(options, &captured.stdout, &captured.stderr) {
        return Ok(ExecResult::from_parts(
            captured.stdout,
            captured.stderr,
            captured.exit_code,
        ));
    }
    let summary = summarize_git_output(subcommand, &captured, options.limit);
    let recovery_hint = crate::tee::tee_raw_output(
        command,
        &captured.stdout,
        &captured.stderr,
        summary.truncated || captured.exit_code != 0,
    );
    render_git(
        command,
        &summary,
        options,
        captured.exit_code,
        &captured.stderr,
        recovery_hint.as_deref(),
    )
}

pub fn summarize_git_output(
    subcommand: GitReadOnly,
    captured: &CapturedCommand,
    limit: usize,
) -> GitSummary {
    let stdout = String::from_utf8_lossy(&captured.stdout);
    let raw_lines: Vec<String> = stdout.lines().map(ToString::to_string).collect();
    let lines = match subcommand {
        GitReadOnly::Status => compact_status(&raw_lines, limit),
        GitReadOnly::Diff | GitReadOnly::Show => compact_patch(&raw_lines, limit),
        GitReadOnly::Log => raw_lines.iter().take(limit).cloned().collect(),
        GitReadOnly::Branch => raw_lines.iter().take(limit).cloned().collect(),
        GitReadOnly::LsFiles | GitReadOnly::LsTree => compact_file_list(&raw_lines, limit),
        GitReadOnly::RevParse
        | GitReadOnly::Remote
        | GitReadOnly::Config
        | GitReadOnly::MergeBase
        | GitReadOnly::Describe
        | GitReadOnly::Blame => raw_lines.iter().take(limit).cloned().collect(),
    };
    let shown_lines = lines.len();
    GitSummary {
        subcommand: subcommand.as_str().to_string(),
        raw_lines: raw_lines.len(),
        shown_lines,
        omitted_lines: raw_lines.len().saturating_sub(shown_lines),
        truncated: shown_lines < raw_lines.len(),
        lines,
    }
}

fn compact_file_list(lines: &[String], limit: usize) -> Vec<String> {
    lines
        .iter()
        .filter(|line| {
            !(line.contains("/node_modules/")
                || line.contains("/target/")
                || line.contains("/vendor/")
                || line.contains("/dist/")
                || line.contains("/build/"))
        })
        .take(limit)
        .cloned()
        .collect()
}

fn compact_status(lines: &[String], limit: usize) -> Vec<String> {
    if lines.is_empty() {
        return vec!["working tree clean or status produced no stdout".to_string()];
    }

    let mut out = Vec::new();
    for line in lines {
        if out.len() >= limit {
            break;
        }
        let trimmed = line.trim();
        if trimmed.starts_with("On branch")
            || trimmed.starts_with("Your branch")
            || trimmed.starts_with("Changes to be committed")
            || trimmed.starts_with("Changes not staged")
            || trimmed.starts_with("Untracked files")
            || trimmed.starts_with("nothing to commit")
            || trimmed.starts_with("modified:")
            || trimmed.starts_with("new file:")
            || trimmed.starts_with("deleted:")
            || trimmed.starts_with("renamed:")
            || trimmed.starts_with("both modified:")
            || trimmed.starts_with("??")
            || trimmed.starts_with("M ")
            || trimmed.starts_with("A ")
            || trimmed.starts_with("D ")
        {
            out.push(line.clone());
        }
    }
    if out.is_empty() {
        out.extend(lines.iter().take(limit).cloned());
    }
    out
}

fn compact_patch(lines: &[String], limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    for line in lines {
        let keep = line.starts_with("diff --git ")
            || line.starts_with("index ")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
            || line.starts_with("@@")
            || line.starts_with('+')
            || line.starts_with('-');
        if keep {
            out.push(line.clone());
        }
        if out.len() >= limit {
            break;
        }
    }
    if out.is_empty() {
        out.extend(lines.iter().take(limit).cloned());
    }
    out
}

fn render_git(
    command: &str,
    summary: &GitSummary,
    options: OutputOptions,
    exit_code: i32,
    stderr: &[u8],
    recovery_hint: Option<&str>,
) -> Result<ExecResult> {
    if options.json {
        return json_result(command, true, exit_code, stderr, summary.truncated, summary);
    }

    let mut out = String::new();
    let mut budget_truncated = false;
    push_budgeted_line(
        &mut out,
        &format!("agentgrep optimized: {command}"),
        options.budget,
        &mut budget_truncated,
    );
    push_budgeted_line(
        &mut out,
        &format!(
            "git {}: {} raw line(s), showing {}.",
            summary.subcommand, summary.raw_lines, summary.shown_lines
        ),
        options.budget,
        &mut budget_truncated,
    );
    for line in &summary.lines {
        if !push_budgeted_line(&mut out, line, options.budget, &mut budget_truncated) {
            break;
        }
    }
    let truncated = summary.truncated || budget_truncated;
    if truncated {
        out.push_str(&format!(
            "Truncated: omitted {} git output line(s). Use --raw for the exact command output.\n",
            summary.omitted_lines.max(1)
        ));
    }
    if let Some(hint) = recovery_hint {
        out.push_str(hint);
        out.push('\n');
    }
    out.push_str(&status_footer(
        exit_code,
        Some(&format!("agentgrep run {command:?} --raw")),
    ));
    Ok(ExecResult::from_parts(
        out.into_bytes(),
        stderr.to_vec(),
        exit_code,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn compact_patch_preserves_file_and_hunk_headers() {
        let captured = CapturedCommand {
            stdout: b"diff --git a/a b/a\nindex 1..2\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n context\n".to_vec(),
            stderr: Vec::new(),
            exit_code: 0,
            duration: Duration::from_millis(1),
        };
        let summary = summarize_git_output(GitReadOnly::Diff, &captured, 20);
        assert!(
            summary
                .lines
                .iter()
                .any(|line| line.starts_with("diff --git"))
        );
        assert!(summary.lines.iter().any(|line| line.starts_with("@@")));
        assert!(summary.lines.iter().any(|line| line == "+new"));
    }
}
