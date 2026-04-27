use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::exec::{command_exists, run_shell_capture};
use crate::output::{ExecResult, OutputOptions, json_result, status_footer};

#[derive(Debug, Clone, Serialize)]
pub struct DoctorSummary {
    pub cwd: String,
    pub git_repo: bool,
    pub ripgrep: ToolStatus,
    pub git: ToolStatus,
    pub sqlite3: ToolStatus,
    pub index_present: bool,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolStatus {
    pub found: bool,
    pub path: Option<String>,
    pub version: Option<String>,
}

pub fn execute_doctor(options: OutputOptions) -> Result<ExecResult> {
    let options = options.normalized();
    let cwd = std::env::current_dir()?;
    let summary = DoctorSummary {
        cwd: cwd.display().to_string(),
        git_repo: is_git_repo(),
        ripgrep: tool_status("rg", "rg --version"),
        git: tool_status("git", "git --version"),
        sqlite3: tool_status("sqlite3", "sqlite3 --version"),
        index_present: Path::new(".agentgrep/index.json").is_file(),
        status: "ok".to_string(),
    };

    if options.json {
        return json_result("agentgrep doctor", true, 0, &[], false, &summary);
    }

    let mut out = String::new();
    out.push_str("agentgrep doctor\n");
    out.push_str(&format!("cwd: {}\n", summary.cwd));
    out.push_str(&format!("git repo: {}\n", yes_no(summary.git_repo)));
    out.push_str(&format_tool("ripgrep", &summary.ripgrep));
    out.push_str(&format_tool("git", &summary.git));
    out.push_str(&format_tool("sqlite3", &summary.sqlite3));
    out.push_str(&format!(
        "index present: {}\n",
        yes_no(summary.index_present)
    ));
    out.push_str("filters: .gitignore + generated/vendor/build/dependency/binary skips\n");
    out.push_str(&format!("status: {}\n", summary.status));
    out.push_str(&status_footer(0, None));
    Ok(ExecResult::success(out.into_bytes()))
}

fn tool_status(name: &str, version_command: &str) -> ToolStatus {
    let path = command_exists(name);
    let version = path.as_ref().and_then(|_| {
        run_shell_capture(version_command, None)
            .ok()
            .and_then(|captured| {
                String::from_utf8(captured.stdout)
                    .ok()
                    .and_then(|stdout| stdout.lines().next().map(ToString::to_string))
            })
    });
    ToolStatus {
        found: path.is_some(),
        path,
        version,
    }
}

fn is_git_repo() -> bool {
    run_shell_capture("git rev-parse --is-inside-work-tree", None)
        .map(|captured| captured.exit_code == 0)
        .unwrap_or(false)
}

fn format_tool(label: &str, status: &ToolStatus) -> String {
    if status.found {
        format!(
            "{label}: {} ({})\n",
            status.path.as_deref().unwrap_or("found"),
            status.version.as_deref().unwrap_or("version unknown")
        )
    } else {
        format!("{label}: missing\n")
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}
