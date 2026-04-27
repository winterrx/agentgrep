use anyhow::Result;
use serde::Serialize;

use crate::command::TestCommand;
use crate::output::{
    ExecResult, OutputOptions, json_result, push_budgeted_line, raw_fits_budget, status_footer,
};

#[derive(Debug, Clone, Serialize)]
pub struct TestSummary {
    pub runner: String,
    pub raw_lines: usize,
    pub shown_lines: usize,
    pub omitted_lines: usize,
    pub truncated: bool,
    pub lines: Vec<String>,
}

pub fn execute_test(
    command: &str,
    runner: TestCommand,
    options: OutputOptions,
) -> Result<ExecResult> {
    let options = options.normalized();
    if options.raw {
        return crate::run::passthrough_real_tools(command);
    }
    let captured = crate::exec::run_shell_capture_real_tools(command, None)?;
    if raw_fits_budget(options, &captured.stdout, &captured.stderr) {
        return Ok(ExecResult::from_parts(
            captured.stdout,
            captured.stderr,
            captured.exit_code,
        ));
    }

    let stdout = String::from_utf8_lossy(&captured.stdout);
    let summary = summarize_test_output(runner, &stdout, options.limit);
    render_test(
        command,
        &summary,
        options,
        captured.exit_code,
        &captured.stderr,
    )
}

fn summarize_test_output(runner: TestCommand, stdout: &str, limit: usize) -> TestSummary {
    let raw_lines: Vec<&str> = stdout.lines().collect();
    let mut lines = Vec::new();
    for line in &raw_lines {
        let trimmed = line.trim();
        if is_high_value_test_line(trimmed) {
            lines.push((*line).to_string());
        }
        if lines.len() >= limit {
            break;
        }
    }
    if lines.is_empty() {
        lines.extend(
            raw_lines
                .iter()
                .rev()
                .filter(|line| !line.trim().is_empty())
                .take(limit)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .map(|line| (*line).to_string()),
        );
    }
    let shown_lines = lines.len();
    TestSummary {
        runner: match runner {
            TestCommand::CargoTest => "cargo test",
            TestCommand::Pytest => "pytest",
            TestCommand::GoTest => "go test",
        }
        .to_string(),
        raw_lines: raw_lines.len(),
        shown_lines,
        omitted_lines: raw_lines.len().saturating_sub(shown_lines),
        truncated: shown_lines < raw_lines.len(),
        lines,
    }
}

fn is_high_value_test_line(line: &str) -> bool {
    line.starts_with("test result:")
        || line.starts_with("running ")
        || line.starts_with("failures:")
        || line.starts_with("FAILED ")
        || line.starts_with("ERROR ")
        || line.starts_with("FAIL ")
        || line.starts_with("--- FAIL:")
        || line.starts_with("FAIL\t")
        || line.starts_with("ok  \t")
        || line.starts_with("PASS")
        || line.starts_with("error:")
        || line.starts_with("error[")
        || line.starts_with("warning:")
        || line.starts_with("thread '")
        || line.starts_with("---- ")
        || line.starts_with('>')
        || line.starts_with("E   ")
        || line.contains(" panicked at ")
        || line.contains("AssertionError")
        || line.contains(".rs:")
        || line.contains(".py:")
        || line.contains(".go:")
}

fn render_test(
    command: &str,
    summary: &TestSummary,
    options: OutputOptions,
    exit_code: i32,
    stderr: &[u8],
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
            "{}: {} raw stdout line(s), showing {}.",
            summary.runner, summary.raw_lines, summary.shown_lines
        ),
        options.budget,
        &mut budget_truncated,
    );
    for line in &summary.lines {
        if !push_budgeted_line(&mut out, line, options.budget, &mut budget_truncated) {
            break;
        }
    }
    if summary.truncated || budget_truncated {
        out.push_str(&format!(
            "Truncated: omitted {} stdout line(s). Stderr is preserved byte-for-byte. Use --raw for exact stdout.\n",
            summary.omitted_lines.max(1)
        ));
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
    fn summarizes_pytest_failure_lines() {
        let output = "tests/a.py .\nFAILED tests/a.py::test_x - AssertionError\n1 failed, 1 passed in 0.10s\n";
        let summary = summarize_test_output(TestCommand::Pytest, output, 8);
        assert!(summary.lines.iter().any(|line| line.contains("FAILED")));
        assert!(summary.truncated);
    }

    #[test]
    fn summarizes_cargo_test_result_lines() {
        let output =
            "running 2 tests\ntest a ... ok\ntest b ... ok\ntest result: ok. 2 passed; 0 failed\n";
        let summary = summarize_test_output(TestCommand::CargoTest, output, 8);
        assert!(
            summary
                .lines
                .iter()
                .any(|line| line.starts_with("running "))
        );
        assert!(
            summary
                .lines
                .iter()
                .any(|line| line.starts_with("test result:"))
        );
    }
}
