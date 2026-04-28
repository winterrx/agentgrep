use anyhow::Result;
use serde::Serialize;

use crate::command::TestCommand;
use crate::output::{
    ExecResult, OutputOptions, json_result, push_budgeted_line, raw_fits_budget, status_footer,
};

pub use crate::parser::{
    ParseResult, ParseTier, parse_jest_json, parse_jest_text, parse_mypy_json, parse_mypy_text,
    parse_playwright_json, parse_playwright_text, parse_ruff_json, parse_ruff_text,
    parse_vitest_json, parse_vitest_text,
};

#[derive(Debug, Clone, Serialize)]
pub struct TestSummary {
    pub runner: String,
    pub raw_lines: usize,
    pub shown_lines: usize,
    pub omitted_lines: usize,
    pub truncated: bool,
    pub parse: ParseResult,
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

    let stdout = String::from_utf8_lossy(&captured.stdout);
    let summary = summarize_test_output(runner, &stdout, options.limit);
    render_test(
        command,
        &summary,
        options,
        captured.exit_code,
        &captured.stderr,
    )
    .map(|result| result.with_baseline_output_tokens(raw_tokens))
}

fn summarize_test_output(runner: TestCommand, stdout: &str, limit: usize) -> TestSummary {
    let parse = match runner {
        TestCommand::CargoTest | TestCommand::CargoCheck | TestCommand::CargoClippy => {
            crate::parser::parse_cargo_test(stdout, limit)
        }
        TestCommand::Pytest => crate::parser::parse_pytest(stdout, limit),
        TestCommand::GoTest => crate::parser::parse_go_test(stdout, limit),
        TestCommand::Vitest => crate::parser::parse_vitest_text(stdout, limit),
        TestCommand::Jest => crate::parser::parse_jest_text(stdout, limit),
        TestCommand::Playwright => crate::parser::parse_playwright_text(stdout, limit),
        TestCommand::Ruff => crate::parser::parse_ruff_text(stdout, limit),
        TestCommand::Mypy => crate::parser::parse_mypy_text(stdout, limit),
        TestCommand::Npm | TestCommand::Pnpm | TestCommand::Yarn => {
            crate::parser::parse_vitest_text(stdout, limit)
        }
    };
    TestSummary {
        runner: runner_name(runner).to_string(),
        raw_lines: parse.raw_lines,
        shown_lines: parse.shown_lines,
        omitted_lines: parse.omitted_lines,
        truncated: parse.truncated,
        lines: parse.lines.clone(),
        parse,
    }
}

fn runner_name(runner: TestCommand) -> &'static str {
    match runner {
        TestCommand::CargoTest => "cargo test",
        TestCommand::CargoCheck => "cargo check",
        TestCommand::CargoClippy => "cargo clippy",
        TestCommand::Pytest => "pytest",
        TestCommand::GoTest => "go test",
        TestCommand::Npm => "npm",
        TestCommand::Pnpm => "pnpm",
        TestCommand::Yarn => "yarn",
        TestCommand::Vitest => "vitest",
        TestCommand::Jest => "jest",
        TestCommand::Playwright => "playwright",
        TestCommand::Ruff => "ruff",
        TestCommand::Mypy => "mypy",
    }
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
            "{}: {} parse, {} raw stdout line(s), showing {}.",
            summary.runner,
            parse_tier_label(summary.parse.tier),
            summary.raw_lines,
            summary.shown_lines
        ),
        options.budget,
        &mut budget_truncated,
    );
    for marker in &summary.parse.markers {
        if !push_budgeted_line(&mut out, marker, options.budget, &mut budget_truncated) {
            break;
        }
    }
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

fn parse_tier_label(tier: ParseTier) -> &'static str {
    match tier {
        ParseTier::Full => "full",
        ParseTier::Degraded => "DEGRADED",
        ParseTier::Passthrough => "PASSTHROUGH",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_pytest_failure_lines() {
        let output = "tests/a.py .\nFAILED tests/a.py::test_x - AssertionError\n1 failed, 1 passed in 0.10s\n";
        let summary = summarize_test_output(TestCommand::Pytest, output, 8);
        assert!(summary.lines.iter().any(|line| line.contains("FAILED")));
        assert_eq!(summary.parse.tier, ParseTier::Full);
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

    #[test]
    fn renders_degraded_marker() {
        let output = "running 1 test\nfailures:\n---- tests::x stdout ----\nthread 'tests::x' panicked at src/lib.rs:1:1:\nboom\nextra\nextra\n";
        let summary = summarize_test_output(TestCommand::CargoTest, output, 2);
        let rendered = render_test("cargo test", &summary, OutputOptions::default(), 101, b"")
            .expect("render succeeds");
        let stdout = String::from_utf8(rendered.stdout).expect("utf8");
        assert!(stdout.contains("DEGRADED"));
        assert!(stdout.contains("structured parse exceeded output limit"));
    }

    #[test]
    fn renders_passthrough_marker_and_preserves_stderr() {
        let output = "noise 1\nnoise 2\nnoise 3\nnoise 4\n";
        let summary = summarize_test_output(TestCommand::GoTest, output, 2);
        let rendered = render_test(
            "go test ./...",
            &summary,
            OutputOptions::default(),
            1,
            b"stderr detail",
        )
        .expect("render succeeds");
        let stdout = String::from_utf8(rendered.stdout).expect("utf8");
        assert!(stdout.contains("PASSTHROUGH"));
        assert!(stdout.contains("omitted 2 stdout line"));
        assert_eq!(rendered.stderr, b"stderr detail");
    }
}
