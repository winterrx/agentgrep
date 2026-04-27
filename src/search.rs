use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use regex::Regex;
use serde::Serialize;

use crate::filters::{collect_source_files, is_text_file};
use crate::output::{
    ExecResult, OutputOptions, estimate_tokens, json_result, push_budgeted_line, status_footer,
};

#[derive(Debug, Clone, Serialize)]
pub struct SearchMatch {
    pub path: String,
    pub line_number: usize,
    pub line: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchSummary {
    pub pattern: String,
    pub roots: Vec<String>,
    pub files_searched: usize,
    pub total_matches: usize,
    pub shown_matches: usize,
    pub files_with_matches: usize,
    pub omitted_matches: usize,
    pub truncated: bool,
    pub matches: Vec<SearchMatch>,
    pub errors: Vec<String>,
}

#[derive(Debug)]
enum Matcher {
    Literal(String),
    Regex(Regex),
}

impl Matcher {
    fn new(pattern: &str, exact: bool) -> Result<Self> {
        if exact {
            Ok(Self::Literal(pattern.to_string()))
        } else {
            Ok(Self::Regex(Regex::new(pattern).with_context(|| {
                format!("invalid regex pattern: {pattern}")
            })?))
        }
    }

    fn is_match(&self, line: &str) -> bool {
        match self {
            Self::Literal(needle) => line.contains(needle),
            Self::Regex(regex) => regex.is_match(line),
        }
    }
}

pub fn execute_regex(
    pattern: &str,
    paths: &[PathBuf],
    options: OutputOptions,
    command_label: Option<String>,
) -> Result<ExecResult> {
    let options = options.normalized();
    if options.raw {
        return execute_raw_regex(pattern, paths, options.exact);
    }

    let summary = search_paths(pattern, paths, options.exact, options.limit)?;
    let exit_code = if summary.total_matches == 0 { 1 } else { 0 };
    let command = command_label.unwrap_or_else(|| format!("agentgrep regex {pattern:?}"));
    render_search_result(&summary, options, &command, exit_code, &[], None)
}

fn execute_raw_regex(pattern: &str, paths: &[PathBuf], exact: bool) -> Result<ExecResult> {
    let mut command = String::from("rg --color never");
    if exact {
        command.push_str(" -F");
    }
    command.push(' ');
    command.push_str(&shell_words::quote(pattern));
    for path in paths {
        command.push(' ');
        command.push_str(&shell_words::quote(&path.display().to_string()));
    }
    let captured = crate::exec::run_shell_capture(&command, None)?;
    Ok(ExecResult::from_parts(
        captured.stdout,
        captured.stderr,
        captured.exit_code,
    ))
}

pub fn search_paths(
    pattern: &str,
    roots: &[PathBuf],
    exact: bool,
    limit: usize,
) -> Result<SearchSummary> {
    let roots = if roots.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        roots.to_vec()
    };
    let matcher = Matcher::new(pattern, exact)?;
    let mut files = collect_source_files(&roots);
    files.sort_by_key(|path| (search_rank(path), path.display().to_string()));
    let mut matches = Vec::new();
    let mut errors = Vec::new();
    let mut total_matches = 0;
    let mut files_with_matches = BTreeSet::new();

    for path in &files {
        if !is_text_file(path) {
            continue;
        }
        let content = match fs::read_to_string(path) {
            Ok(content) => content,
            Err(error) => {
                errors.push(format!("{}: {error}", path.display()));
                continue;
            }
        };
        let lines: Vec<&str> = content.lines().collect();
        for (idx, line) in lines.iter().enumerate() {
            if matcher.is_match(line) {
                total_matches += 1;
                files_with_matches.insert(path.display().to_string());
                if matches.len() < limit {
                    matches.push(SearchMatch {
                        path: path.display().to_string(),
                        line_number: idx + 1,
                        line: (*line).to_string(),
                        before: idx
                            .checked_sub(1)
                            .and_then(|before| lines.get(before))
                            .map(|s| (*s).to_string()),
                        after: lines.get(idx + 1).map(|s| (*s).to_string()),
                    });
                }
            }
        }
    }

    let shown_matches = matches.len();
    Ok(SearchSummary {
        pattern: pattern.to_string(),
        roots: roots.iter().map(|p| p.display().to_string()).collect(),
        files_searched: files.len(),
        total_matches,
        shown_matches,
        files_with_matches: files_with_matches.len(),
        omitted_matches: total_matches.saturating_sub(shown_matches),
        truncated: shown_matches < total_matches,
        matches,
        errors,
    })
}

fn search_rank(path: &Path) -> u8 {
    let value = path.to_string_lossy();
    if value.starts_with("src/") || value.contains("/src/") || value.starts_with("./src/") {
        0
    } else if value.starts_with("tests/")
        || value.contains("/tests/")
        || value.starts_with("./tests/")
    {
        1
    } else if value.ends_with(".md") || value.ends_with(".txt") {
        2
    } else if value.ends_with(".json") || value.ends_with(".jsonl") || value.ends_with(".lock") {
        3
    } else {
        4
    }
}

pub fn parse_raw_match_lines(raw_stdout: &[u8], limit: usize) -> Vec<SearchMatch> {
    let stdout = String::from_utf8_lossy(raw_stdout);
    let mut matches = Vec::new();
    for line in stdout.lines() {
        if matches.len() >= limit {
            break;
        }
        if let Some(parsed) = parse_path_line_match(line) {
            matches.push(parsed);
        }
    }
    matches
}

pub fn summary_from_raw_match_lines(
    pattern: &str,
    roots: &[PathBuf],
    raw_stdout: &[u8],
    limit: usize,
) -> Option<SearchSummary> {
    let stdout = String::from_utf8_lossy(raw_stdout);
    let mut matches = Vec::new();
    let mut total_matches = 0;
    for line in stdout.lines() {
        let Some(parsed) = parse_path_line_match(line) else {
            continue;
        };
        total_matches += 1;
        if matches.len() < limit {
            matches.push(parsed);
        }
    }

    if total_matches == 0 {
        return None;
    }

    Some(summary_from_matches(
        pattern,
        roots,
        0,
        total_matches,
        matches,
    ))
}

fn parse_path_line_match(line: &str) -> Option<SearchMatch> {
    let mut parts = line.splitn(3, ':');
    let path = parts.next()?;
    let second = parts.next()?;
    let (line_number, matched_line) = match second.parse().ok() {
        Some(line_number) => (line_number, parts.next()?.to_string()),
        None => {
            let matched_line = match parts.next() {
                Some(rest) => format!("{second}:{rest}"),
                None => second.to_string(),
            };
            let line_number = find_line_number(Path::new(path), &matched_line)?;
            (line_number, matched_line)
        }
    };
    Some(SearchMatch {
        path: path.to_string(),
        line_number,
        line: matched_line,
        before: context_line(Path::new(path), line_number.saturating_sub(1)),
        after: context_line(Path::new(path), line_number + 1),
    })
}

fn find_line_number(path: &Path, needle: &str) -> Option<usize> {
    let content = fs::read_to_string(path).ok()?;
    content
        .lines()
        .position(|line| line == needle)
        .map(|idx| idx + 1)
}

fn context_line(path: &Path, line_number: usize) -> Option<String> {
    if line_number == 0 {
        return None;
    }
    let content = fs::read_to_string(path).ok()?;
    content
        .lines()
        .nth(line_number - 1)
        .map(ToString::to_string)
}

pub fn summary_from_matches(
    pattern: &str,
    roots: &[PathBuf],
    files_searched: usize,
    total_matches: usize,
    matches: Vec<SearchMatch>,
) -> SearchSummary {
    let mut by_file = BTreeMap::<String, usize>::new();
    for item in &matches {
        *by_file.entry(item.path.clone()).or_default() += 1;
    }
    SearchSummary {
        pattern: pattern.to_string(),
        roots: roots.iter().map(|p| p.display().to_string()).collect(),
        files_searched,
        total_matches,
        shown_matches: matches.len(),
        files_with_matches: by_file.len(),
        omitted_matches: total_matches.saturating_sub(matches.len()),
        truncated: matches.len() < total_matches,
        matches,
        errors: Vec::new(),
    }
}

pub fn render_search_result(
    summary: &SearchSummary,
    options: OutputOptions,
    command: &str,
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
    let plural = if summary.total_matches == 1 {
        "match"
    } else {
        "matches"
    };
    push_budgeted_line(
        &mut out,
        &format!(
            "{} {plural} across {} files. Showing {}.",
            summary.total_matches, summary.files_with_matches, summary.shown_matches
        ),
        options.budget,
        &mut budget_truncated,
    );

    if summary.total_matches == 0 {
        push_budgeted_line(
            &mut out,
            "No matches found.",
            options.budget,
            &mut budget_truncated,
        );
    }

    for item in &summary.matches {
        if estimate_tokens(&out) >= options.budget {
            budget_truncated = true;
            break;
        }
        let header = format!("{}:{}", item.path, item.line_number);
        if !push_budgeted_line(&mut out, &header, options.budget, &mut budget_truncated) {
            break;
        }
        if let Some(before) = &item.before {
            let before_line = format!("  | {}", before.trim_end());
            if !push_budgeted_line(
                &mut out,
                &before_line,
                options.budget,
                &mut budget_truncated,
            ) {
                break;
            }
        }
        let matched = format!("> | {}", item.line.trim_end());
        if !push_budgeted_line(&mut out, &matched, options.budget, &mut budget_truncated) {
            break;
        }
        if let Some(after) = &item.after {
            let after_line = format!("  | {}", after.trim_end());
            if !push_budgeted_line(&mut out, &after_line, options.budget, &mut budget_truncated) {
                break;
            }
        }
        push_budgeted_line(&mut out, "", options.budget, &mut budget_truncated);
    }

    let truncated = summary.truncated || budget_truncated;
    if truncated {
        let omitted = summary.omitted_matches.max(1);
        out.push_str(&format!(
            "Truncated: omitted at least {omitted} match(es). Use --limit, --budget, or --raw for more.\n"
        ));
    }
    if let Some(hint) = recovery_hint {
        out.push_str(hint);
        out.push('\n');
    }
    for error in &summary.errors {
        out.push_str("Error: ");
        out.push_str(error);
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

    #[test]
    fn parses_rg_style_raw_lines() {
        let raw = b"src/main.rs:12:let stripe = true;\n";
        let parsed = parse_raw_match_lines(raw, 10);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].path, "src/main.rs");
        assert_eq!(parsed[0].line_number, 12);
        assert_eq!(parsed[0].line, "let stripe = true;");
    }
}
