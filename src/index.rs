use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::filters::{collect_source_files, is_text_file};
use crate::output::{ExecResult, OutputOptions, json_result, status_footer};

const INDEX_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize)]
pub struct IndexSummary {
    pub version: u32,
    pub root: String,
    pub index_path: String,
    pub files: usize,
    pub bytes: u64,
    pub trigrams: usize,
    pub skipped_binary_or_unreadable: usize,
}

#[derive(Debug, Clone, Serialize)]
struct IndexDocument {
    version: u32,
    root: String,
    created_unix: u64,
    files: Vec<IndexedFile>,
}

#[derive(Debug, Clone, Serialize)]
struct IndexedFile {
    path: String,
    bytes: u64,
    modified_unix: u64,
    trigrams: Vec<String>,
}

pub fn execute_index(path: &Path, options: OutputOptions) -> Result<ExecResult> {
    let options = options.normalized();
    let (summary, document) = build_index(path)?;
    let index_dir = path.join(".agentgrep");
    fs::create_dir_all(&index_dir)
        .with_context(|| format!("failed to create {}", index_dir.display()))?;
    let index_path = index_dir.join("index.json");
    let bytes = serde_json::to_vec_pretty(&document)?;
    fs::write(&index_path, bytes)
        .with_context(|| format!("failed to write {}", index_path.display()))?;

    if options.json {
        return json_result(
            format!("agentgrep index {}", path.display()),
            true,
            0,
            &[],
            false,
            &summary,
        );
    }

    let mut out = String::new();
    out.push_str(&format!("agentgrep index: {}\n", path.display()));
    out.push_str(&format!(
        "Indexed {} files, {} bytes, {} unique trigram(s).\n",
        summary.files, summary.bytes, summary.trigrams
    ));
    if summary.skipped_binary_or_unreadable > 0 {
        out.push_str(&format!(
            "Skipped {} binary or unreadable file(s).\n",
            summary.skipped_binary_or_unreadable
        ));
    }
    out.push_str(&format!("Wrote: {}\n", summary.index_path));
    out.push_str(&status_footer(0, None));
    Ok(ExecResult::success(out.into_bytes()))
}

fn build_index(path: &Path) -> Result<(IndexSummary, IndexDocument)> {
    let files = collect_source_files(&[path.to_path_buf()]);
    let mut indexed = Vec::new();
    let mut all_trigrams = BTreeSet::new();
    let mut total_bytes = 0;
    let mut skipped = 0;

    for file in files {
        if !is_text_file(&file) {
            skipped += 1;
            continue;
        }
        let Ok(content) = fs::read_to_string(&file) else {
            skipped += 1;
            continue;
        };
        let metadata = fs::metadata(&file)?;
        let modified_unix = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let trigrams = trigrams(&content);
        for trigram in &trigrams {
            all_trigrams.insert(trigram.clone());
        }
        total_bytes += metadata.len();
        indexed.push(IndexedFile {
            path: relative(path, &file),
            bytes: metadata.len(),
            modified_unix,
            trigrams,
        });
    }

    let index_path = path.join(".agentgrep/index.json");
    let summary = IndexSummary {
        version: INDEX_VERSION,
        root: path.display().to_string(),
        index_path: index_path.display().to_string(),
        files: indexed.len(),
        bytes: total_bytes,
        trigrams: all_trigrams.len(),
        skipped_binary_or_unreadable: skipped,
    };
    let document = IndexDocument {
        version: INDEX_VERSION,
        root: path.display().to_string(),
        created_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        files: indexed,
    };
    Ok((summary, document))
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .ok()
        .filter(|relative| !relative.as_os_str().is_empty())
        .unwrap_or(path)
        .display()
        .to_string()
}

fn trigrams(content: &str) -> Vec<String> {
    let mut set = BTreeSet::new();
    let chars: Vec<char> = content.chars().collect();
    for window in chars.windows(3).take(20_000) {
        let trigram: String = window.iter().collect();
        if !trigram.chars().any(char::is_control) {
            set.insert(trigram);
        }
    }
    set.into_iter().take(2000).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_deduped_trigrams() {
        assert_eq!(trigrams("stripe stripe").len(), 7);
    }
}
