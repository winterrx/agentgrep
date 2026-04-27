use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::command::{FindCommand, FindNamePattern};
use crate::filters::collect_source_files;
use crate::output::{ExecResult, OutputOptions, json_result, push_budgeted_line, status_footer};

#[derive(Debug, Clone, Serialize)]
pub struct RepoMapSummary {
    pub root: String,
    pub total_files: usize,
    pub shown_files: usize,
    pub omitted_files: usize,
    pub truncated: bool,
    pub directories: Vec<DirectorySummary>,
    pub files: Vec<String>,
    pub filters: Vec<&'static str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub query_filters: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DirectorySummary {
    pub path: String,
    pub files: usize,
}

pub fn execute_map(
    path: &Path,
    options: OutputOptions,
    command_label: Option<String>,
) -> Result<ExecResult> {
    execute_map_with_recovery(path, options, command_label, None)
}

pub fn execute_map_with_recovery(
    path: &Path,
    options: OutputOptions,
    command_label: Option<String>,
    recovery_hint: Option<&str>,
) -> Result<ExecResult> {
    let options = options.normalized();
    let summary = build_map(path, options.limit);
    let command = command_label.unwrap_or_else(|| format!("agentgrep map {}", path.display()));
    render_map(&summary, options, &command, 0, &[], recovery_hint)
}

pub fn execute_find_map(
    query: &FindCommand,
    options: OutputOptions,
    command_label: Option<String>,
) -> Result<ExecResult> {
    let options = options.normalized();
    let summary = build_find_map(query, options.limit);
    let command =
        command_label.unwrap_or_else(|| format!("agentgrep map {}", query.path.display()));
    render_map(&summary, options, &command, 0, &[], None)
}

pub fn build_map(path: &Path, limit: usize) -> RepoMapSummary {
    build_map_with_query(path, limit, None)
}

pub fn build_find_map(query: &FindCommand, limit: usize) -> RepoMapSummary {
    build_map_with_query(&query.path, limit, Some(query))
}

fn build_map_with_query(path: &Path, limit: usize, query: Option<&FindCommand>) -> RepoMapSummary {
    let root = path.to_path_buf();
    let mut files = collect_source_files(std::slice::from_ref(&root));
    if let Some(query) = query {
        files.retain(|file| matches_find_query(path, file, query));
    }
    let mut directories = BTreeMap::<String, usize>::new();
    for file in &files {
        let relative = relative_display(path, file);
        let parent = Path::new(&relative)
            .parent()
            .map(|parent| parent.display().to_string())
            .filter(|parent| !parent.is_empty())
            .unwrap_or_else(|| ".".to_string());
        *directories.entry(parent).or_default() += 1;
    }

    let shown_files = files.len().min(limit);
    RepoMapSummary {
        root: path.display().to_string(),
        total_files: files.len(),
        shown_files,
        omitted_files: files.len().saturating_sub(shown_files),
        truncated: shown_files < files.len(),
        directories: directories
            .into_iter()
            .take(limit)
            .map(|(path, files)| DirectorySummary { path, files })
            .collect(),
        files: files
            .iter()
            .take(limit)
            .map(|file| relative_display(path, file))
            .collect(),
        filters: vec![
            ".gitignore",
            "hidden files",
            "generated files",
            "vendor/dependencies",
            "build output",
            "lockfiles",
            "binary/media files",
        ],
        query_filters: query.map(find_query_filters).unwrap_or_default(),
    }
}

fn matches_find_query(root: &Path, file: &Path, query: &FindCommand) -> bool {
    let depth = find_depth(root, file);
    if let Some(min_depth) = query.min_depth
        && depth < min_depth
    {
        return false;
    }
    if let Some(max_depth) = query.max_depth
        && depth > max_depth
    {
        return false;
    }
    if query.name_patterns.is_empty() {
        return true;
    }
    let Some(name) = file.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    query
        .name_patterns
        .iter()
        .all(|pattern| matches_find_name(pattern, name))
}

fn find_depth(root: &Path, file: &Path) -> usize {
    file.strip_prefix(root)
        .ok()
        .filter(|relative| !relative.as_os_str().is_empty())
        .unwrap_or(file)
        .components()
        .count()
}

fn matches_find_name(pattern: &FindNamePattern, name: &str) -> bool {
    if pattern.case_insensitive {
        wildcard_matches(&pattern.pattern.to_lowercase(), &name.to_lowercase())
    } else {
        wildcard_matches(&pattern.pattern, name)
    }
}

fn wildcard_matches(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let mut matches = vec![vec![false; text.len() + 1]; pattern.len() + 1];
    matches[0][0] = true;
    for i in 1..=pattern.len() {
        if pattern[i - 1] == '*' {
            matches[i][0] = matches[i - 1][0];
        }
    }
    for i in 1..=pattern.len() {
        for j in 1..=text.len() {
            matches[i][j] = match pattern[i - 1] {
                '*' => matches[i - 1][j] || matches[i][j - 1],
                '?' => matches[i - 1][j - 1],
                ch => matches[i - 1][j - 1] && ch == text[j - 1],
            };
        }
    }
    matches[pattern.len()][text.len()]
}

fn find_query_filters(query: &FindCommand) -> Vec<String> {
    let mut filters = Vec::new();
    for pattern in &query.name_patterns {
        let label = if pattern.case_insensitive {
            "iname"
        } else {
            "name"
        };
        filters.push(format!("{label}={}", pattern.pattern));
    }
    if let Some(depth) = query.min_depth {
        filters.push(format!("mindepth={depth}"));
    }
    if let Some(depth) = query.max_depth {
        filters.push(format!("maxdepth={depth}"));
    }
    filters
}

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .ok()
        .filter(|relative| !relative.as_os_str().is_empty())
        .unwrap_or(path)
        .display()
        .to_string()
}

pub fn render_map(
    summary: &RepoMapSummary,
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
    let header = if summary.truncated {
        format!(
            "{} files under {}. Showing {}. Filters: ignored/hidden/generated/vendor/build/deps/binary.",
            summary.total_files, summary.root, summary.shown_files
        )
    } else {
        format!(
            "{} files under {}. Filters: ignored/hidden/generated/vendor/build/deps/binary.",
            summary.total_files, summary.root
        )
    };
    push_budgeted_line(&mut out, &header, options.budget, &mut budget_truncated);
    if !summary.query_filters.is_empty() {
        push_budgeted_line(
            &mut out,
            &format!("Find filters: {}", summary.query_filters.join(", ")),
            options.budget,
            &mut budget_truncated,
        );
    }

    if summary.truncated && !summary.directories.is_empty() {
        push_budgeted_line(
            &mut out,
            "Directories:",
            options.budget,
            &mut budget_truncated,
        );
        for directory in &summary.directories {
            let line = format!("  {} ({})", directory.path, directory.files);
            if !push_budgeted_line(&mut out, &line, options.budget, &mut budget_truncated) {
                break;
            }
        }
    }

    if summary.truncated {
        push_budgeted_line(&mut out, "Files:", options.budget, &mut budget_truncated);
    }
    for file in &summary.files {
        let line = format!("  {file}");
        if !push_budgeted_line(&mut out, &line, options.budget, &mut budget_truncated) {
            break;
        }
    }

    let truncated = summary.truncated || budget_truncated;
    if truncated {
        out.push_str(&format!(
            "Truncated: omitted {} file(s). Use --limit or --budget for more.\n",
            summary.omitted_files.max(1)
        ));
    }
    if let Some(hint) = recovery_hint {
        out.push_str(hint);
        out.push('\n');
    }
    out.push_str(&status_footer(exit_code, None));

    Ok(ExecResult::from_parts(
        out.into_bytes(),
        stderr.to_vec(),
        exit_code,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn map_skips_vendor_and_build_outputs() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::create_dir_all(tmp.path().join("node_modules/pkg")).unwrap();
        fs::create_dir_all(tmp.path().join("target/debug")).unwrap();
        fs::write(tmp.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(tmp.path().join("node_modules/pkg/index.js"), "stripe\n").unwrap();
        fs::write(tmp.path().join("target/debug/out.txt"), "stripe\n").unwrap();

        let summary = build_map(tmp.path(), 20);
        assert_eq!(summary.total_files, 1);
        assert_eq!(summary.files, vec!["src/main.rs"]);
    }

    #[test]
    fn find_map_honors_name_and_depth_filters() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("src/deep")).unwrap();
        fs::write(tmp.path().join("README.md"), "docs\n").unwrap();
        fs::write(tmp.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(tmp.path().join("src/deep/mod.rs"), "mod deep;\n").unwrap();
        fs::write(tmp.path().join("src/deep/note.txt"), "note\n").unwrap();

        let summary = build_find_map(
            &FindCommand {
                path: tmp.path().to_path_buf(),
                name_patterns: vec![FindNamePattern {
                    pattern: "*.rs".to_string(),
                    case_insensitive: false,
                }],
                min_depth: None,
                max_depth: Some(2),
            },
            20,
        );

        assert_eq!(summary.files, vec!["src/main.rs"]);
        assert_eq!(summary.query_filters, vec!["name=*.rs", "maxdepth=2"]);
    }

    #[test]
    fn find_map_supports_case_insensitive_name_patterns() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("README.MD"), "docs\n").unwrap();
        fs::write(tmp.path().join("notes.txt"), "note\n").unwrap();

        let summary = build_find_map(
            &FindCommand {
                path: tmp.path().to_path_buf(),
                name_patterns: vec![FindNamePattern {
                    pattern: "*.md".to_string(),
                    case_insensitive: true,
                }],
                min_depth: None,
                max_depth: None,
            },
            20,
        );

        assert_eq!(summary.files, vec!["README.MD"]);
    }
}
