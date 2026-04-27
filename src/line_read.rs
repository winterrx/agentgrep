use std::fs;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::command::{FileSliceCommand, FileSliceRange};
use crate::exec::run_shell_capture;
use crate::file_view;
use crate::output::{
    ExecResult, OutputOptions, json_result, push_budgeted_line, raw_fits_budget, status_footer,
};

#[derive(Debug, Clone, Serialize)]
pub struct LineCountSummary {
    pub paths: Vec<LineCountEntry>,
    pub total_lines: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct LineCountEntry {
    pub path: String,
    pub lines: usize,
}

pub fn execute_file_slice(
    command: &str,
    slice: FileSliceCommand,
    options: OutputOptions,
) -> Result<ExecResult> {
    let raw = run_shell_capture(command, None)?;
    if raw.exit_code != 0
        || !slice.path.is_file()
        || raw_fits_budget(options.normalized(), &raw.stdout, &raw.stderr)
    {
        return Ok(ExecResult::from_parts(
            raw.stdout,
            raw.stderr,
            raw.exit_code,
        ));
    }

    let range = match range_spec(&slice)? {
        Some(range) => range,
        None => {
            return Ok(ExecResult::from_parts(
                raw.stdout,
                raw.stderr,
                raw.exit_code,
            ));
        }
    };
    file_view::execute_file_with_label(&slice.path, Some(&range), options, Some(command))
}

pub fn execute_wc_lines(
    command: &str,
    paths: Vec<std::path::PathBuf>,
    options: OutputOptions,
) -> Result<ExecResult> {
    let options = options.normalized();
    let raw = run_shell_capture(command, None)?;
    if raw.exit_code != 0
        || paths.iter().any(|path| !path.is_file())
        || raw_fits_budget(options, &raw.stdout, &raw.stderr)
    {
        return Ok(ExecResult::from_parts(
            raw.stdout,
            raw.stderr,
            raw.exit_code,
        ));
    }
    let mut entries = Vec::new();
    for path in paths {
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        entries.push(LineCountEntry {
            path: path.display().to_string(),
            lines: content.lines().count(),
        });
    }
    let total_lines = entries.iter().map(|entry| entry.lines).sum();
    let total_entries = entries.len();
    let shown = total_entries.min(options.limit);
    let summary = LineCountSummary {
        paths: entries.into_iter().take(shown).collect(),
        total_lines,
        truncated: shown < total_entries,
    };

    if options.json {
        return json_result(
            command,
            true,
            raw.exit_code,
            &raw.stderr,
            summary.truncated,
            &summary,
        );
    }

    let mut out = String::new();
    let mut budget_truncated = false;
    push_budgeted_line(
        &mut out,
        &format!("agentgrep optimized: {command}"),
        options.budget,
        &mut budget_truncated,
    );
    for entry in &summary.paths {
        let line = format!("{:>8} {}", entry.lines, entry.path);
        if !push_budgeted_line(&mut out, &line, options.budget, &mut budget_truncated) {
            break;
        }
    }
    if summary.paths.len() > 1 {
        let line = format!("{:>8} total", summary.total_lines);
        push_budgeted_line(&mut out, &line, options.budget, &mut budget_truncated);
    }
    if summary.truncated || budget_truncated {
        out.push_str("Truncated: omitted line-count entries. Use --raw for exact wc output.\n");
    }
    out.push_str(&status_footer(
        raw.exit_code,
        Some(&format!("agentgrep run {command:?} --raw")),
    ));
    Ok(ExecResult::from_parts(
        out.into_bytes(),
        raw.stderr,
        raw.exit_code,
    ))
}

fn range_spec(slice: &FileSliceCommand) -> Result<Option<String>> {
    let line_count = fs::read_to_string(&slice.path)
        .with_context(|| format!("failed to read {}", slice.path.display()))?
        .lines()
        .count();
    if line_count == 0 {
        return Ok(None);
    }
    let (start, end) = match slice.range {
        FileSliceRange::FirstLines(lines) => (1, lines.min(line_count)),
        FileSliceRange::LastLines(lines) => {
            let start = line_count.saturating_sub(lines).saturating_add(1);
            (start.max(1), line_count)
        }
        FileSliceRange::Explicit { start, end } => (start.min(line_count), end.min(line_count)),
    };
    Ok(Some(format!("{}:{}", start.max(1), end.max(start))))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::command::{FileSliceKind, FileSliceRange};

    #[test]
    fn computes_tail_range() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("sample.txt");
        fs::write(&file, "a\nb\nc\nd\n").unwrap();
        let range = range_spec(&FileSliceCommand {
            kind: FileSliceKind::Tail,
            path: PathBuf::from(&file),
            range: FileSliceRange::LastLines(2),
        })
        .unwrap();
        assert_eq!(range.as_deref(), Some("3:4"));
    }
}
