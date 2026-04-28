use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ParseTier {
    Full,
    Degraded,
    Passthrough,
}

#[derive(Debug, Clone, Serialize)]
pub struct ParseResult {
    pub parser: String,
    pub tier: ParseTier,
    pub raw_lines: usize,
    pub shown_lines: usize,
    pub omitted_lines: usize,
    pub truncated: bool,
    pub markers: Vec<String>,
    pub lines: Vec<String>,
}

impl ParseResult {
    pub fn new(
        parser: impl Into<String>,
        tier: ParseTier,
        raw_lines: usize,
        lines: Vec<String>,
        markers: Vec<String>,
    ) -> Self {
        let shown_lines = lines.len();
        Self {
            parser: parser.into(),
            tier,
            raw_lines,
            shown_lines,
            omitted_lines: raw_lines.saturating_sub(shown_lines),
            truncated: shown_lines < raw_lines,
            markers,
            lines,
        }
    }

    pub fn passthrough(parser: impl Into<String>, stdout: &str, limit: usize) -> Self {
        let raw_lines: Vec<&str> = stdout.lines().collect();
        let lines = raw_lines
            .iter()
            .rev()
            .filter(|line| !line.trim().is_empty())
            .take(limit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|line| (*line).to_string())
            .collect::<Vec<_>>();
        let mut result = Self::new(
            parser,
            ParseTier::Passthrough,
            raw_lines.len(),
            lines,
            vec!["PASSTHROUGH: no structured test/lint summary recognized".to_string()],
        );
        result.truncated = result.shown_lines < result.raw_lines;
        result.omitted_lines = result.raw_lines.saturating_sub(result.shown_lines);
        result
    }

    fn degraded(mut self, reason: impl Into<String>) -> Self {
        self.tier = ParseTier::Degraded;
        self.markers.push(format!("DEGRADED: {}", reason.into()));
        self
    }
}

pub fn parse_cargo_test(stdout: &str, limit: usize) -> ParseResult {
    let raw_lines: Vec<&str> = stdout.lines().collect();
    let mut ranges = Vec::new();

    for (idx, line) in raw_lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("running ")
            || trimmed.starts_with("test result:")
            || trimmed.starts_with("error:")
            || trimmed.starts_with("error[")
            || trimmed.starts_with("warning:")
            || trimmed.starts_with("failures:")
        {
            ranges.push((
                idx,
                idx.saturating_add(3).min(raw_lines.len().saturating_sub(1)),
            ));
        }
        if trimmed.starts_with("---- ") || trimmed.starts_with("thread '") {
            ranges.push((idx, block_end(&raw_lines, idx, &["---- ", "test result:"])));
        }
        if trimmed.contains(" panicked at ") || trimmed.contains(".rs:") {
            ranges.push((
                idx.saturating_sub(2),
                idx.saturating_add(2).min(raw_lines.len().saturating_sub(1)),
            ));
        }
    }

    structured_or_passthrough("cargo test", stdout, &raw_lines, ranges, limit)
}

pub fn parse_pytest(stdout: &str, limit: usize) -> ParseResult {
    let raw_lines: Vec<&str> = stdout.lines().collect();
    let mut ranges = Vec::new();

    for (idx, line) in raw_lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("FAILED ")
            || trimmed.starts_with("ERROR ")
            || trimmed.starts_with("====")
            || trimmed.starts_with("____")
            || trimmed.starts_with("E   ")
            || trimmed.contains("AssertionError")
            || trimmed.contains(".py:")
            || trimmed.contains(" failed")
            || trimmed.contains(" passed")
            || trimmed.contains(" error")
        {
            ranges.push((
                idx.saturating_sub(2),
                idx.saturating_add(4).min(raw_lines.len().saturating_sub(1)),
            ));
        }
    }

    structured_or_passthrough("pytest", stdout, &raw_lines, ranges, limit)
}

pub fn parse_go_test(stdout: &str, limit: usize) -> ParseResult {
    let raw_lines: Vec<&str> = stdout.lines().collect();
    let mut ranges = Vec::new();

    for (idx, line) in raw_lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("--- FAIL:")
            || trimmed.starts_with("FAIL")
            || trimmed.starts_with("ok  \t")
            || trimmed.starts_with("PASS")
        {
            ranges.push((
                idx,
                block_end(
                    &raw_lines,
                    idx,
                    &["=== RUN", "--- PASS:", "--- FAIL:", "ok  \t", "FAIL"],
                ),
            ));
        }
        if trimmed.contains(".go:") {
            ranges.push((
                idx.saturating_sub(2),
                idx.saturating_add(2).min(raw_lines.len().saturating_sub(1)),
            ));
        }
    }

    structured_or_passthrough("go test", stdout, &raw_lines, ranges, limit)
}

pub fn parse_vitest_text(stdout: &str, limit: usize) -> ParseResult {
    parse_js_test_text("vitest", stdout, limit)
}

pub fn parse_jest_text(stdout: &str, limit: usize) -> ParseResult {
    parse_js_test_text("jest", stdout, limit)
}

pub fn parse_playwright_text(stdout: &str, limit: usize) -> ParseResult {
    let raw_lines: Vec<&str> = stdout.lines().collect();
    let mut ranges = Vec::new();
    for (idx, line) in raw_lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("Error:")
            || trimmed.starts_with("Timeout")
            || trimmed.starts_with("Running ")
            || trimmed.starts_with("Failed ")
            || trimmed.starts_with("Flaky ")
            || trimmed.starts_with("Passed ")
            || trimmed.contains(" tests failed")
            || trimmed.contains("Test timeout")
        {
            ranges.push((
                idx.saturating_sub(2),
                idx.saturating_add(5).min(raw_lines.len().saturating_sub(1)),
            ));
        }
    }
    structured_or_passthrough("playwright", stdout, &raw_lines, ranges, limit)
}

pub fn parse_ruff_text(stdout: &str, limit: usize) -> ParseResult {
    parse_diagnostic_text(
        "ruff",
        stdout,
        limit,
        &[".py:", "Found ", "All checks passed"],
    )
}

pub fn parse_mypy_text(stdout: &str, limit: usize) -> ParseResult {
    parse_diagnostic_text(
        "mypy",
        stdout,
        limit,
        &[": error:", ": note:", "Success:", "Found "],
    )
}

pub fn parse_vitest_json(stdout: &str, limit: usize) -> ParseResult {
    parse_json_summary("vitest-json", stdout, limit)
}

pub fn parse_jest_json(stdout: &str, limit: usize) -> ParseResult {
    parse_json_summary("jest-json", stdout, limit)
}

pub fn parse_playwright_json(stdout: &str, limit: usize) -> ParseResult {
    parse_json_summary("playwright-json", stdout, limit)
}

pub fn parse_ruff_json(stdout: &str, limit: usize) -> ParseResult {
    parse_json_summary("ruff-json", stdout, limit)
}

pub fn parse_mypy_json(stdout: &str, limit: usize) -> ParseResult {
    parse_json_summary("mypy-json", stdout, limit)
}

fn parse_js_test_text(parser: &str, stdout: &str, limit: usize) -> ParseResult {
    let raw_lines: Vec<&str> = stdout.lines().collect();
    let mut ranges = Vec::new();
    for (idx, line) in raw_lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("FAIL ")
            || trimmed.starts_with("PASS ")
            || trimmed.starts_with("Test Files")
            || trimmed.starts_with("Tests")
            || trimmed.starts_with("Snapshots")
            || trimmed.starts_with("Error:")
            || trimmed.contains("AssertionError")
            || trimmed.contains("Expected")
            || trimmed.contains("Received")
        {
            ranges.push((
                idx.saturating_sub(2),
                idx.saturating_add(4).min(raw_lines.len().saturating_sub(1)),
            ));
        }
    }
    structured_or_passthrough(parser, stdout, &raw_lines, ranges, limit)
}

fn parse_diagnostic_text(
    parser: &str,
    stdout: &str,
    limit: usize,
    needles: &[&str],
) -> ParseResult {
    let raw_lines: Vec<&str> = stdout.lines().collect();
    let mut ranges = Vec::new();
    for (idx, line) in raw_lines.iter().enumerate() {
        if needles.iter().any(|needle| line.contains(needle)) {
            ranges.push((
                idx,
                idx.saturating_add(2).min(raw_lines.len().saturating_sub(1)),
            ));
        }
    }
    structured_or_passthrough(parser, stdout, &raw_lines, ranges, limit)
}

fn parse_json_summary(parser: &str, stdout: &str, limit: usize) -> ParseResult {
    let raw_lines = stdout.lines().count();
    let value = match serde_json::from_str::<Value>(stdout) {
        Ok(value) => value,
        Err(error) => {
            return ParseResult::passthrough(parser, stdout, limit)
                .degraded(format!("invalid JSON: {error}"));
        }
    };

    let mut lines = Vec::new();
    collect_json_lines("", &value, &mut lines, limit);
    if lines.is_empty() {
        return ParseResult::new(
            parser,
            ParseTier::Degraded,
            raw_lines,
            vec!["JSON parsed but no recognizable summary fields were found".to_string()],
            vec!["DEGRADED: parsed JSON without known test/lint fields".to_string()],
        );
    }

    let mut result = ParseResult::new(parser, ParseTier::Full, raw_lines, lines, Vec::new());
    if result.shown_lines >= limit {
        result = result.degraded("JSON summary exceeded output limit");
        result.truncated = true;
    }
    result
}

fn collect_json_lines(prefix: &str, value: &Value, lines: &mut Vec<String>, limit: usize) {
    if lines.len() >= limit {
        return;
    }
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if lines.len() >= limit {
                    return;
                }
                let next_prefix = if prefix.is_empty() {
                    key.to_string()
                } else {
                    format!("{prefix}.{key}")
                };
                let interesting = matches!(
                    key.as_str(),
                    "numFailedTests"
                        | "numPassedTests"
                        | "numTotalTests"
                        | "numFailedTestSuites"
                        | "numPassedTestSuites"
                        | "status"
                        | "message"
                        | "title"
                        | "path"
                        | "file"
                        | "line"
                        | "column"
                        | "code"
                        | "severity"
                        | "messageText"
                        | "summary"
                        | "errors"
                        | "failures"
                );
                if interesting && !value.is_object() && !value.is_array() {
                    lines.push(format!("{next_prefix}: {}", scalar_to_string(value)));
                } else {
                    collect_json_lines(&next_prefix, value, lines, limit);
                }
            }
        }
        Value::Array(values) => {
            for value in values.iter().take(limit.saturating_sub(lines.len())) {
                collect_json_lines(prefix, value, lines, limit);
            }
        }
        _ => {}
    }
}

fn scalar_to_string(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

fn structured_or_passthrough(
    parser: &str,
    stdout: &str,
    raw_lines: &[&str],
    ranges: Vec<(usize, usize)>,
    limit: usize,
) -> ParseResult {
    let mut lines = Vec::new();
    for (start, end) in ranges {
        for idx in start..=end {
            if let Some(line) = raw_lines.get(idx) {
                push_unique_line(&mut lines, line, limit);
            }
            if lines.len() >= limit {
                break;
            }
        }
        if lines.len() >= limit {
            break;
        }
    }

    if lines.is_empty() {
        return ParseResult::passthrough(parser, stdout, limit);
    }

    let tier = if lines.len() >= limit && raw_lines.len() > lines.len() {
        ParseTier::Degraded
    } else {
        ParseTier::Full
    };
    let markers = if tier == ParseTier::Degraded {
        vec!["DEGRADED: structured parse exceeded output limit".to_string()]
    } else {
        Vec::new()
    };
    ParseResult::new(parser, tier, raw_lines.len(), lines, markers)
}

fn push_unique_line(lines: &mut Vec<String>, line: &str, limit: usize) {
    if lines.len() >= limit {
        return;
    }
    let line = line.to_string();
    if !lines.contains(&line) {
        lines.push(line);
    }
}

fn block_end(raw_lines: &[&str], start: usize, stop_prefixes: &[&str]) -> usize {
    let mut end = start;
    for (idx, line) in raw_lines.iter().enumerate().skip(start.saturating_add(1)) {
        let trimmed = line.trim();
        if idx > start.saturating_add(1)
            && stop_prefixes
                .iter()
                .any(|prefix| trimmed.starts_with(prefix))
        {
            break;
        }
        end = idx;
        if trimmed.is_empty() && idx > start.saturating_add(2) {
            break;
        }
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_failure_keeps_panic_detail() {
        let output = "running 1 test\ntest tests::fails ... FAILED\n\nfailures:\n\n---- tests::fails stdout ----\nthread 'tests::fails' panicked at src/lib.rs:7:9:\nassertion failed: left == right\n\nfailures:\n    tests::fails\n\ntest result: FAILED. 0 passed; 1 failed\n";
        let parsed = parse_cargo_test(output, 20);
        assert_eq!(parsed.tier, ParseTier::Full);
        assert!(
            parsed
                .lines
                .iter()
                .any(|line| line.contains("panicked at src/lib.rs"))
        );
        assert!(
            parsed
                .lines
                .iter()
                .any(|line| line.contains("assertion failed"))
        );
        assert!(
            parsed
                .lines
                .iter()
                .any(|line| line.starts_with("test result: FAILED"))
        );
    }

    #[test]
    fn pytest_failure_keeps_error_context() {
        let output = "tests/a.py F\n\n________________________________ test_x ________________________________\n\n    def test_x():\n>       assert 1 == 2\nE       assert 1 == 2\n\ntests/a.py:3: AssertionError\n=========================== short test summary info ===========================\nFAILED tests/a.py::test_x - AssertionError\n1 failed in 0.01s\n";
        let parsed = parse_pytest(output, 12);
        assert!(
            parsed
                .lines
                .iter()
                .any(|line| line.contains(">       assert"))
        );
        assert!(
            parsed
                .lines
                .iter()
                .any(|line| line.contains("FAILED tests/a.py::test_x"))
        );
    }

    #[test]
    fn passthrough_marks_unrecognized_and_truncates() {
        let output = "noise 1\nnoise 2\nnoise 3\nnoise 4\n";
        let parsed = parse_go_test(output, 2);
        assert_eq!(parsed.tier, ParseTier::Passthrough);
        assert!(parsed.truncated);
        assert_eq!(
            parsed.lines,
            vec!["noise 3".to_string(), "noise 4".to_string()]
        );
        assert!(
            parsed
                .markers
                .iter()
                .any(|marker| marker.contains("PASSTHROUGH"))
        );
    }

    #[test]
    fn malformed_json_degrades_to_passthrough_marker() {
        let parsed = parse_jest_json("{ nope", 4);
        assert_eq!(parsed.tier, ParseTier::Degraded);
        assert!(
            parsed
                .markers
                .iter()
                .any(|marker| marker.contains("invalid JSON"))
        );
    }

    #[test]
    fn jest_json_extracts_summary_fields() {
        let output = r#"{"numFailedTests":1,"numPassedTests":2,"testResults":[{"name":"tests/a.test.ts","message":"Expected true to be false"}]}"#;
        let parsed = parse_jest_json(output, 8);
        assert_eq!(parsed.tier, ParseTier::Full);
        assert!(
            parsed
                .lines
                .iter()
                .any(|line| line.contains("numFailedTests: 1"))
        );
        assert!(
            parsed
                .lines
                .iter()
                .any(|line| line.contains("Expected true"))
        );
    }

    #[test]
    fn mypy_json_extracts_diagnostics() {
        let output = r#"[{"file":"pkg/a.py","line":4,"column":8,"message":"Incompatible return value type","severity":"error"}]"#;
        let parsed = parse_mypy_json(output, 8);
        assert_eq!(parsed.tier, ParseTier::Full);
        assert!(parsed.lines.iter().any(|line| line.contains("pkg/a.py")));
        assert!(
            parsed
                .lines
                .iter()
                .any(|line| line.contains("Incompatible return"))
        );
    }
}
