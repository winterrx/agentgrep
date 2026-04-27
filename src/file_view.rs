use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::Serialize;

use crate::filters::is_text_file;
use crate::output::{
    ExecResult, OutputOptions, json_result, push_budgeted_line, raw_fits_budget, status_footer,
};

const LARGE_FILE_BYTES: usize = 16 * 1024;
const LARGE_FILE_LINES: usize = 260;

#[derive(Debug, Clone, Serialize)]
pub struct FileSummary {
    pub path: String,
    pub bytes: usize,
    pub lines: usize,
    pub mode: FileMode,
    pub range: Option<String>,
    pub shown_lines: Vec<NumberedLine>,
    pub imports: Vec<NumberedLine>,
    pub symbols: Vec<NumberedLine>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileMode {
    Full,
    Range,
    Summary,
}

#[derive(Debug, Clone, Serialize)]
pub struct NumberedLine {
    pub line_number: usize,
    pub text: String,
}

pub fn execute_file(
    path: &Path,
    lines: Option<&str>,
    options: OutputOptions,
) -> Result<ExecResult> {
    execute_file_with_label(path, lines, options, None)
}

pub fn execute_file_with_label(
    path: &Path,
    lines: Option<&str>,
    options: OutputOptions,
    command_label: Option<&str>,
) -> Result<ExecResult> {
    let options = options.normalized();
    if options.raw {
        return raw_file(path);
    }
    if command_label.is_none()
        && lines.is_none()
        && let Ok(content) = fs::read_to_string(path)
        && content.len() <= LARGE_FILE_BYTES
        && content.lines().count() <= LARGE_FILE_LINES
        && raw_fits_budget(options, content.as_bytes(), &[])
    {
        return Ok(ExecResult::from_parts(content.into_bytes(), Vec::new(), 0));
    }

    match build_file_summary(path, lines, options.limit) {
        Ok(summary) => render_file_summary(&summary, options, 0, &[], command_label),
        Err(error) => Ok(ExecResult::from_parts(
            Vec::new(),
            format!("{error:#}\n").into_bytes(),
            1,
        )),
    }
}

fn raw_file(path: &Path) -> Result<ExecResult> {
    match fs::read(path) {
        Ok(bytes) => Ok(ExecResult::from_parts(bytes, Vec::new(), 0)),
        Err(error) => Ok(ExecResult::from_parts(
            Vec::new(),
            format!("cat: {}: {error}\n", path.display()).into_bytes(),
            1,
        )),
    }
}

pub fn build_file_summary(path: &Path, lines: Option<&str>, limit: usize) -> Result<FileSummary> {
    if !path.is_file() {
        bail!("{} is not a file", path.display());
    }
    if !is_text_file(path) {
        bail!("{} looks binary; use --raw for exact bytes", path.display());
    }

    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let all_lines: Vec<&str> = content.lines().collect();
    let bytes = content.len();
    let line_count = all_lines.len();

    if let Some(range) = lines {
        let (start, end) = parse_line_range(range, line_count)?;
        let shown_end = end.min(start.saturating_add(limit.max(1)).saturating_sub(1));
        return Ok(FileSummary {
            path: path.display().to_string(),
            bytes,
            lines: line_count,
            mode: FileMode::Range,
            range: if shown_end < end {
                Some(format!("{start}:{shown_end} of requested {start}:{end}"))
            } else {
                Some(format!("{start}:{end}"))
            },
            shown_lines: numbered_slice(&all_lines, start, shown_end),
            imports: Vec::new(),
            symbols: Vec::new(),
            truncated: shown_end < end || end < line_count || start > 1,
        });
    }

    let large = bytes > LARGE_FILE_BYTES || line_count > LARGE_FILE_LINES;
    if !large {
        return Ok(FileSummary {
            path: path.display().to_string(),
            bytes,
            lines: line_count,
            mode: FileMode::Full,
            range: Some(format!("1:{line_count}")),
            shown_lines: numbered_slice(&all_lines, 1, line_count.min(limit.max(1))),
            imports: Vec::new(),
            symbols: Vec::new(),
            truncated: line_count > limit,
        });
    }

    let imports = extract_imports(&all_lines, 20);
    let symbols = extract_symbols(&all_lines, limit.min(80));
    let mut shown_lines = Vec::new();
    let head = line_count.min(24);
    shown_lines.extend(numbered_slice(&all_lines, 1, head));
    if line_count > 48 {
        shown_lines.extend(numbered_slice(
            &all_lines,
            line_count.saturating_sub(23),
            line_count,
        ));
    }

    Ok(FileSummary {
        path: path.display().to_string(),
        bytes,
        lines: line_count,
        mode: FileMode::Summary,
        range: None,
        shown_lines,
        imports,
        symbols,
        truncated: true,
    })
}

fn parse_line_range(range: &str, line_count: usize) -> Result<(usize, usize)> {
    let Some((start, end)) = range.split_once(':') else {
        bail!("line range must look like start:end");
    };
    let start = start.parse::<usize>().context("invalid range start")?;
    let end = end.parse::<usize>().context("invalid range end")?;
    if start == 0 || end == 0 || start > end {
        bail!("line range must be 1-based and start <= end");
    }
    Ok((start.min(line_count.max(1)), end.min(line_count.max(1))))
}

fn numbered_slice(lines: &[&str], start: usize, end: usize) -> Vec<NumberedLine> {
    if lines.is_empty() {
        return Vec::new();
    }
    let start = start.max(1);
    let end = end.min(lines.len());
    if start > end {
        return Vec::new();
    }
    lines[start - 1..end]
        .iter()
        .enumerate()
        .map(|(offset, text)| NumberedLine {
            line_number: start + offset,
            text: (*text).to_string(),
        })
        .collect()
}

fn extract_imports(lines: &[&str], limit: usize) -> Vec<NumberedLine> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| {
            let trimmed = line.trim_start();
            trimmed.starts_with("use ")
                || trimmed.starts_with("pub use ")
                || trimmed.starts_with("mod ")
                || trimmed.starts_with("pub mod ")
                || trimmed.starts_with("import ")
                || trimmed.starts_with("from ")
                || trimmed.starts_with("const ") && trimmed.contains("require(")
        })
        .take(limit)
        .map(|(idx, text)| NumberedLine {
            line_number: idx + 1,
            text: (*text).to_string(),
        })
        .collect()
}

fn extract_symbols(lines: &[&str], limit: usize) -> Vec<NumberedLine> {
    let symbol = Regex::new(
        r"^\s*(pub\s+)?(async\s+)?(fn|struct|enum|trait|impl|class|def|function|const|let|type|interface)\s+[A-Za-z0-9_]+",
    )
    .expect("symbol regex compiles");
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| symbol.is_match(line))
        .take(limit)
        .map(|(idx, text)| NumberedLine {
            line_number: idx + 1,
            text: (*text).to_string(),
        })
        .collect()
}

pub fn render_file_summary(
    summary: &FileSummary,
    options: OutputOptions,
    exit_code: i32,
    stderr: &[u8],
    command_label: Option<&str>,
) -> Result<ExecResult> {
    let command = command_label
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("agentgrep file {}", summary.path));
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
        &format!("{} bytes, {} lines.", summary.bytes, summary.lines),
        options.budget,
        &mut budget_truncated,
    );

    match summary.mode {
        FileMode::Full | FileMode::Range => {
            if let Some(range) = &summary.range {
                push_budgeted_line(
                    &mut out,
                    &format!("Showing lines {range}."),
                    options.budget,
                    &mut budget_truncated,
                );
            }
            render_lines(
                &mut out,
                &summary.shown_lines,
                options.budget,
                &mut budget_truncated,
            );
        }
        FileMode::Summary => {
            push_budgeted_line(
                &mut out,
                "Summary mode: large file compacted by default.",
                options.budget,
                &mut budget_truncated,
            );
            if !summary.imports.is_empty() {
                push_budgeted_line(
                    &mut out,
                    "Imports/modules:",
                    options.budget,
                    &mut budget_truncated,
                );
                render_lines(
                    &mut out,
                    &summary.imports,
                    options.budget,
                    &mut budget_truncated,
                );
            }
            if !summary.symbols.is_empty() {
                push_budgeted_line(&mut out, "Symbols:", options.budget, &mut budget_truncated);
                render_lines(
                    &mut out,
                    &summary.symbols,
                    options.budget,
                    &mut budget_truncated,
                );
            }
            push_budgeted_line(&mut out, "Edges:", options.budget, &mut budget_truncated);
            render_lines(
                &mut out,
                &summary.shown_lines,
                options.budget,
                &mut budget_truncated,
            );
        }
    }

    let truncated = summary.truncated || budget_truncated;
    if truncated {
        out.push_str("Truncated: file content omitted. Use --lines, --limit, --budget, or --raw for exact output.\n");
    }
    out.push_str(&status_footer(
        exit_code,
        Some(&format!("agentgrep file {} --raw", summary.path)),
    ));

    Ok(ExecResult::from_parts(
        out.into_bytes(),
        stderr.to_vec(),
        exit_code,
    ))
}

fn render_lines(
    out: &mut String,
    lines: &[NumberedLine],
    budget: usize,
    budget_truncated: &mut bool,
) {
    for line in lines {
        let rendered = format!("{:>5} | {}", line.line_number, line.text);
        if !push_budgeted_line(out, &rendered, budget, budget_truncated) {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn summarizes_large_files() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("big.rs");
        let mut content = String::from("use std::fs;\n\npub struct Billing {}\n");
        for i in 0..320 {
            content.push_str(&format!("fn stripe_{i}() {{}}\n"));
        }
        fs::write(&file, content).unwrap();

        let summary = build_file_summary(&file, None, 20).unwrap();
        assert!(matches!(summary.mode, FileMode::Summary));
        assert!(!summary.symbols.is_empty());
        assert!(summary.truncated);
    }

    #[test]
    fn supports_line_ranges() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("small.txt");
        fs::write(&file, "a\nb\nc\nd\n").unwrap();

        let summary = build_file_summary(&file, Some("2:3"), 20).unwrap();
        assert!(matches!(summary.mode, FileMode::Range));
        assert_eq!(summary.shown_lines[0].line_number, 2);
        assert_eq!(summary.shown_lines[0].text, "b");
    }
}
