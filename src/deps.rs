use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::output::{ExecResult, OutputOptions, json_result, push_budgeted_line, status_footer};

#[derive(Debug, Clone, Serialize)]
pub struct DepsSummary {
    pub root: String,
    pub manifests: Vec<ManifestSummary>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ManifestSummary {
    pub kind: String,
    pub path: String,
    pub lines: Vec<String>,
}

pub fn execute_deps(path: &Path, options: OutputOptions) -> Result<ExecResult> {
    let options = options.normalized();
    if options.raw {
        return crate::run::passthrough_real_tools(&format!(
            "cat {}",
            shell_words::quote(&path.display().to_string())
        ));
    }

    let root = if path.is_file() {
        path.parent().unwrap_or_else(|| Path::new("."))
    } else {
        path
    };
    let mut summary = DepsSummary {
        root: root.display().to_string(),
        manifests: Vec::new(),
        truncated: false,
    };

    add_manifest(&mut summary, root.join("Cargo.toml"), summarize_cargo)?;
    add_manifest(
        &mut summary,
        root.join("package.json"),
        summarize_package_json,
    )?;
    add_manifest(
        &mut summary,
        root.join("requirements.txt"),
        summarize_requirements,
    )?;
    add_manifest(
        &mut summary,
        root.join("pyproject.toml"),
        summarize_pyproject,
    )?;
    add_manifest(&mut summary, root.join("go.mod"), summarize_go_mod)?;

    if options.json {
        return json_result("agentgrep deps", true, 0, &[], summary.truncated, &summary);
    }

    let mut out = String::new();
    let mut budget_truncated = false;
    push_budgeted_line(
        &mut out,
        &format!("agentgrep deps: {}", root.display()),
        options.budget,
        &mut budget_truncated,
    );
    if summary.manifests.is_empty() {
        push_budgeted_line(
            &mut out,
            "No dependency manifests found.",
            options.budget,
            &mut budget_truncated,
        );
    }
    for manifest in &summary.manifests {
        if !push_budgeted_line(
            &mut out,
            &format!("{} ({})", manifest.kind, manifest.path),
            options.budget,
            &mut budget_truncated,
        ) {
            break;
        }
        for line in manifest.lines.iter().take(options.limit) {
            if !push_budgeted_line(
                &mut out,
                &format!("  {line}"),
                options.budget,
                &mut budget_truncated,
            ) {
                break;
            }
        }
        if manifest.lines.len() > options.limit {
            out.push_str(&format!(
                "  ... +{} more dependency line(s)\n",
                manifest.lines.len() - options.limit
            ));
            summary.truncated = true;
        }
    }
    if summary.truncated || budget_truncated {
        out.push_str("Truncated: dependency summary omitted entries. Use --limit/--budget or read the manifest raw.\n");
    }
    out.push_str(&status_footer(0, None));
    Ok(ExecResult::success(out))
}

fn add_manifest<F>(summary: &mut DepsSummary, path: PathBuf, summarize: F) -> Result<()>
where
    F: FnOnce(&Path) -> Result<Option<ManifestSummary>>,
{
    if path.is_file()
        && let Some(manifest) = summarize(&path)?
    {
        summary.manifests.push(manifest);
    }
    Ok(())
}

fn summarize_cargo(path: &Path) -> Result<Option<ManifestSummary>> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut section = String::new();
    let mut lines = Vec::new();
    for raw in content.lines() {
        let line = raw.trim();
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            section = name.to_string();
            continue;
        }
        if !matches!(
            section.as_str(),
            "dependencies" | "dev-dependencies" | "build-dependencies"
        ) || line.is_empty()
            || line.starts_with('#')
        {
            continue;
        }
        if let Some((name, value)) = line.split_once('=') {
            lines.push(format!(
                "{}: {}",
                section_label(&section),
                format_dep_value(name.trim(), value.trim())
            ));
        }
    }
    Ok(Some(ManifestSummary {
        kind: "Rust Cargo.toml".to_string(),
        path: path.display().to_string(),
        lines,
    }))
}

fn summarize_package_json(path: &Path) -> Result<Option<ManifestSummary>> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    let mut lines = Vec::new();
    for key in ["dependencies", "devDependencies", "peerDependencies"] {
        if let Some(deps) = value.get(key).and_then(serde_json::Value::as_object) {
            for (name, version) in deps {
                lines.push(format!(
                    "{}: {} {}",
                    key,
                    name,
                    version.as_str().unwrap_or("*")
                ));
            }
        }
    }
    Ok(Some(ManifestSummary {
        kind: "Node package.json".to_string(),
        path: path.display().to_string(),
        lines,
    }))
}

fn summarize_requirements(path: &Path) -> Result<Option<ManifestSummary>> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let lines = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| format!("package: {line}"))
        .collect();
    Ok(Some(ManifestSummary {
        kind: "Python requirements.txt".to_string(),
        path: path.display().to_string(),
        lines,
    }))
}

fn summarize_pyproject(path: &Path) -> Result<Option<ManifestSummary>> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut lines = Vec::new();
    let mut in_dependencies = false;
    for raw in content.lines() {
        let line = raw.trim();
        if line.starts_with("dependencies") && line.contains('[') {
            in_dependencies = true;
            continue;
        }
        if in_dependencies {
            if line.starts_with(']') {
                in_dependencies = false;
                continue;
            }
            let dep = line.trim_matches(|c| c == '"' || c == '\'' || c == ',');
            if !dep.is_empty() {
                lines.push(format!("dependency: {dep}"));
            }
        }
    }
    Ok(Some(ManifestSummary {
        kind: "Python pyproject.toml".to_string(),
        path: path.display().to_string(),
        lines,
    }))
}

fn summarize_go_mod(path: &Path) -> Result<Option<ManifestSummary>> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut lines = Vec::new();
    let mut in_require = false;
    for raw in content.lines() {
        let line = raw.trim();
        if line.starts_with("module ") || line.starts_with("go ") {
            lines.push(line.to_string());
        } else if line == "require (" {
            in_require = true;
        } else if line == ")" {
            in_require = false;
        } else if in_require || line.starts_with("require ") {
            let dep = line.trim_start_matches("require ").trim();
            if !dep.is_empty() && !dep.starts_with("//") {
                lines.push(format!("require: {dep}"));
            }
        }
    }
    Ok(Some(ManifestSummary {
        kind: "Go go.mod".to_string(),
        path: path.display().to_string(),
        lines,
    }))
}

fn section_label(section: &str) -> &'static str {
    match section {
        "dependencies" => "dep",
        "dev-dependencies" => "dev",
        "build-dependencies" => "build",
        _ => "dep",
    }
}

fn format_dep_value(name: &str, value: &str) -> String {
    if let Some(version) = value.strip_prefix('"').and_then(|v| v.split('"').next()) {
        format!("{name} {version}")
    } else if let Some(version_pos) = value.find("version") {
        let version = &value[version_pos..];
        let version = version
            .split('"')
            .nth(1)
            .map(|v| format!(" {v}"))
            .unwrap_or_default();
        format!("{name}{version}")
    } else {
        name.to_string()
    }
}
