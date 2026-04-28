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

    if subcommand == GitReadOnly::Status
        && let Some(porcelain_command) = status_porcelain_command(command)
    {
        return execute_status_porcelain(command, &porcelain_command, options);
    }
    if subcommand == GitReadOnly::Log
        && let Some(compact_command) = log_compact_command(command, options.limit)
    {
        return execute_log_compact(command, &compact_command, options);
    }

    let captured = crate::exec::run_shell_capture_real_tools(command, None)?;
    if raw_fits_budget(options, &captured.stdout, &captured.stderr) {
        let tokens = crate::output::estimate_tokens_from_bytes(
            captured.stdout.len() + captured.stderr.len(),
        );
        return Ok(
            ExecResult::from_parts(captured.stdout, captured.stderr, captured.exit_code)
                .with_baseline_output_tokens(tokens),
        );
    }
    let raw_tokens =
        crate::output::estimate_tokens_from_bytes(captured.stdout.len() + captured.stderr.len());
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
    .map(|result| result.with_baseline_output_tokens(raw_tokens))
}

fn execute_log_compact(
    command: &str,
    compact_command: &str,
    options: OutputOptions,
) -> Result<ExecResult> {
    let captured = crate::exec::run_shell_capture_real_tools(compact_command, None)?;
    if captured.exit_code != 0 {
        return Ok(ExecResult::from_parts(
            captured.stdout,
            captured.stderr,
            captured.exit_code,
        ));
    }
    let raw_tokens =
        crate::output::estimate_tokens_from_bytes(captured.stdout.len() + captured.stderr.len());
    let text = String::from_utf8_lossy(&captured.stdout);
    let lines = compact_log_output(&text, options.limit, false);
    let summary = GitSummary {
        subcommand: "log".to_string(),
        raw_lines: text.lines().count(),
        shown_lines: lines.len(),
        omitted_lines: 0,
        truncated: false,
        lines,
    };
    render_git(
        command,
        &summary,
        options,
        captured.exit_code,
        &captured.stderr,
        None,
    )
    .map(|result| result.with_baseline_output_tokens(raw_tokens))
}

fn execute_status_porcelain(
    command: &str,
    porcelain_command: &str,
    options: OutputOptions,
) -> Result<ExecResult> {
    let captured = crate::exec::run_shell_capture_real_tools(porcelain_command, None)?;
    if captured.exit_code != 0 {
        return Ok(ExecResult::from_parts(
            captured.stdout,
            captured.stderr,
            captured.exit_code,
        ));
    }
    let raw_tokens =
        crate::output::estimate_tokens_from_bytes(captured.stdout.len() + captured.stderr.len());
    let lines = compact_porcelain_status(&captured.stdout, options.limit);
    let shown_lines = lines.len();
    let summary = GitSummary {
        subcommand: "status".to_string(),
        raw_lines: String::from_utf8_lossy(&captured.stdout).lines().count(),
        shown_lines,
        omitted_lines: 0,
        truncated: false,
        lines,
    };
    render_git(
        command,
        &summary,
        options,
        captured.exit_code,
        &captured.stderr,
        None,
    )
    .map(|result| result.with_baseline_output_tokens(raw_tokens))
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

fn status_porcelain_command(command: &str) -> Option<String> {
    let words = shell_words::split(command).ok()?;
    let status_index = words.iter().position(|word| word == "status")?;
    if words[status_index + 1..]
        .iter()
        .any(|word| word.as_str() != "--")
    {
        return None;
    }

    let mut compact = words[..status_index].to_vec();
    compact.extend([
        "status".to_string(),
        "--porcelain".to_string(),
        "-b".to_string(),
    ]);
    Some(shell_join(&compact))
}

fn log_compact_command(command: &str, limit: usize) -> Option<String> {
    let words = shell_words::split(command).ok()?;
    let log_index = words.iter().position(|word| word == "log")?;
    let args = &words[log_index + 1..];
    if args.iter().any(|arg| {
        arg == "--oneline"
            || arg.starts_with("--pretty")
            || arg.starts_with("--format")
            || arg == "--raw"
            || arg == "--patch"
            || arg == "-p"
    }) {
        return None;
    }

    let has_limit = args.iter().any(|arg| {
        (arg.starts_with('-')
            && arg
                .get(1..2)
                .is_some_and(|c| c.chars().all(|c| c.is_ascii_digit())))
            || arg == "-n"
            || arg.starts_with("--max-count")
    });
    let wants_merges = args
        .iter()
        .any(|arg| arg == "--merges" || arg == "--min-parents=2");

    let mut compact = words[..=log_index].to_vec();
    compact.push("--pretty=format:%h %s (%cr) <%an>%n%b%n---END---".to_string());
    if !has_limit {
        compact.push(format!("-{}", limit.max(1)));
    }
    if !wants_merges {
        compact.push("--no-merges".to_string());
    }
    compact.extend(args.iter().cloned());
    Some(shell_join(&compact))
}

fn shell_join(words: &[String]) -> String {
    words
        .iter()
        .map(|word| shell_words::quote(word).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

fn compact_porcelain_status(stdout: &[u8], limit: usize) -> Vec<String> {
    let text = String::from_utf8_lossy(stdout);
    let mut branch = None;
    let mut staged = Vec::new();
    let mut modified = Vec::new();
    let mut untracked = Vec::new();
    let mut conflicts = Vec::new();

    for line in text.lines() {
        if let Some(value) = line.strip_prefix("## ") {
            branch = Some(format!("* {value}"));
            continue;
        }
        if line.len() < 3 {
            continue;
        }
        let status = &line[..2];
        let file = line[3..].to_string();
        let index = status.as_bytes()[0] as char;
        let worktree = status.as_bytes()[1] as char;

        if matches!(status, "DD" | "AU" | "UD" | "UA" | "DU" | "AA" | "UU")
            || index == 'U'
            || worktree == 'U'
        {
            conflicts.push(file.clone());
            continue;
        }
        if matches!(index, 'M' | 'A' | 'D' | 'R' | 'C') {
            staged.push(file.clone());
        }
        if matches!(worktree, 'M' | 'D') {
            modified.push(file.clone());
        }
        if status == "??" {
            untracked.push(file);
        }
    }

    let mut out = Vec::new();
    if let Some(branch) = branch {
        out.push(branch);
    }
    push_status_group(&mut out, "+ Staged", &staged, limit);
    push_status_group(&mut out, "~ Modified", &modified, limit);
    push_status_group(&mut out, "? Untracked", &untracked, limit);
    push_status_group(&mut out, "! Conflicts", &conflicts, limit);
    if out.len() == branch_line_count(&out) {
        out.push("clean - nothing to commit".to_string());
    }
    if out.is_empty() {
        out.push("Clean working tree".to_string());
    }
    out
}

fn branch_line_count(lines: &[String]) -> usize {
    usize::from(lines.first().is_some_and(|line| line.starts_with("* ")))
}

fn push_status_group(out: &mut Vec<String>, label: &str, files: &[String], limit: usize) {
    if files.is_empty() {
        return;
    }
    out.push(format!("{label}: {} file(s)", files.len()));
    let remaining_budget = limit.saturating_sub(out.len()).max(1);
    for file in files.iter().take(remaining_budget) {
        out.push(format!("  {file}"));
    }
    if files.len() > remaining_budget {
        out.push(format!("  ... +{} more", files.len() - remaining_budget));
    }
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

fn compact_log_output(output: &str, limit: usize, user_set_limit: bool) -> Vec<String> {
    let truncate_width = if user_set_limit { 120 } else { 80 };
    let mut out = Vec::new();
    for block in output.split("---END---").take(limit) {
        let block = block.trim();
        if block.is_empty() {
            continue;
        }
        let mut lines = block.lines();
        let Some(header) = lines.next() else {
            continue;
        };
        out.push(truncate_chars(header.trim(), truncate_width));
        let body_lines = lines
            .map(str::trim)
            .filter(|line| {
                !line.is_empty()
                    && !line.starts_with("Signed-off-by:")
                    && !line.starts_with("Co-authored-by:")
            })
            .collect::<Vec<_>>();
        for body in body_lines.iter().take(3) {
            out.push(format!("  {}", truncate_chars(body, truncate_width)));
        }
        if body_lines.len() > 3 {
            out.push(format!(
                "  [+{} body line(s) omitted]",
                body_lines.len() - 3
            ));
        }
    }
    out
}

fn truncate_chars(line: &str, width: usize) -> String {
    if line.chars().count() <= width {
        line.to_string()
    } else {
        format!(
            "{}...",
            line.chars()
                .take(width.saturating_sub(3))
                .collect::<String>()
        )
    }
}

fn compact_patch(lines: &[String], limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut omitted_context = false;
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
        } else if !line.is_empty() {
            omitted_context = true;
        }
        if out.len() >= limit {
            break;
        }
    }
    if omitted_context && out.len() < limit {
        out.push(
            "Context: unchanged patch context lines omitted; use --raw for exact patch."
                .to_string(),
        );
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

    #[test]
    fn compact_porcelain_status_groups_changes_without_hints() {
        let lines = compact_porcelain_status(
            b"## main...origin/main\n M src/lib.rs\nA  src/new.rs\n?? tmp/\nUU src/conflict.rs\n",
            20,
        );

        assert_eq!(
            lines,
            vec![
                "* main...origin/main",
                "+ Staged: 1 file(s)",
                "  src/new.rs",
                "~ Modified: 1 file(s)",
                "  src/lib.rs",
                "? Untracked: 1 file(s)",
                "  tmp/",
                "! Conflicts: 1 file(s)",
                "  src/conflict.rs",
            ]
        );
    }

    #[test]
    fn status_porcelain_command_preserves_git_globals() {
        assert_eq!(
            status_porcelain_command("git -C ../repo status").unwrap(),
            "git -C ../repo status --porcelain -b"
        );
        assert!(status_porcelain_command("git status --short").is_none());
    }

    #[test]
    fn log_compact_command_respects_user_formats() {
        let command = log_compact_command("git -C ../repo log", 8).unwrap();
        assert!(command.contains("--pretty=format:"));
        assert!(command.contains("-8"));
        assert!(command.contains("--no-merges"));
        assert!(log_compact_command("git log --oneline -n 20", 8).is_none());
        assert!(log_compact_command("git log --format='%h %s'", 8).is_none());
    }

    #[test]
    fn compact_log_output_keeps_header_and_body_context() {
        let output = "abc1234 subject line (2 days ago) <alice>\nbody one\nbody two\nbody three\nbody four\n---END---\n";
        let lines = compact_log_output(output, 8, false);
        assert_eq!(lines[0], "abc1234 subject line (2 days ago) <alice>");
        assert_eq!(lines[1], "  body one");
        assert!(lines.iter().any(|line| line.contains("omitted")));
    }
}
