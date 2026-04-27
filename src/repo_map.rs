use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde::Serialize;

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

pub fn build_map(path: &Path, limit: usize) -> RepoMapSummary {
    let root = path.to_path_buf();
    let files = collect_source_files(std::slice::from_ref(&root));
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
    }
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
}
