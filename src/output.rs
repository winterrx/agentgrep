use serde::Serialize;
use serde_json::Value;
use std::env;

#[derive(Debug, Clone, Copy)]
pub struct OutputOptions {
    pub raw: bool,
    pub json: bool,
    pub exact: bool,
    pub limit: usize,
    pub budget: usize,
}

impl Default for OutputOptions {
    fn default() -> Self {
        Self {
            raw: false,
            json: false,
            exact: false,
            limit: 8,
            budget: 4000,
        }
    }
}

impl OutputOptions {
    pub fn normalized(self) -> Self {
        Self {
            limit: self.limit.max(1),
            budget: self.budget.max(1),
            ..self
        }
    }

    pub fn from_env_defaults() -> Self {
        let mut options = Self::default();
        options.raw = env_flag("AGENTGREP_RAW").unwrap_or(options.raw);
        options.json = env_flag("AGENTGREP_JSON").unwrap_or(options.json);
        options.exact = env_flag("AGENTGREP_EXACT").unwrap_or(options.exact);
        options.limit = env_usize("AGENTGREP_LIMIT").unwrap_or(options.limit);
        options.budget = env_usize("AGENTGREP_BUDGET").unwrap_or(options.budget);
        options
    }
}

fn env_flag(name: &str) -> Option<bool> {
    match env::var(name).ok()?.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn env_usize(name: &str) -> Option<usize> {
    env::var(name).ok()?.parse().ok()
}

#[derive(Debug, Clone)]
pub struct ExecResult {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: i32,
    pub baseline_output_tokens: Option<usize>,
}

impl ExecResult {
    pub fn success(stdout: impl Into<Vec<u8>>) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: Vec::new(),
            exit_code: 0,
            baseline_output_tokens: None,
        }
    }

    pub fn from_parts(
        stdout: impl Into<Vec<u8>>,
        stderr: impl Into<Vec<u8>>,
        exit_code: i32,
    ) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: stderr.into(),
            exit_code,
            baseline_output_tokens: None,
        }
    }

    pub fn with_baseline_output_tokens(mut self, tokens: usize) -> Self {
        self.baseline_output_tokens = Some(tokens);
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Envelope {
    pub command: String,
    pub optimized: bool,
    pub exit_code: i32,
    pub truncated: bool,
    pub stderr: String,
    pub summary: Value,
}

pub fn json_result<T: Serialize>(
    command: impl Into<String>,
    optimized: bool,
    exit_code: i32,
    stderr: &[u8],
    truncated: bool,
    summary: &T,
) -> anyhow::Result<ExecResult> {
    let value = serde_json::to_value(summary)?;
    let envelope = Envelope {
        command: command.into(),
        optimized,
        exit_code,
        truncated,
        stderr: String::from_utf8_lossy(stderr).into_owned(),
        summary: value,
    };
    let mut stdout = serde_json::to_vec_pretty(&envelope)?;
    stdout.push(b'\n');
    Ok(ExecResult::from_parts(stdout, stderr.to_vec(), exit_code))
}

pub fn estimate_tokens_from_bytes(bytes: usize) -> usize {
    bytes.div_ceil(4)
}

pub fn estimate_tokens(text: &str) -> usize {
    estimate_tokens_from_bytes(text.len())
}

pub fn raw_fits_budget(options: OutputOptions, stdout: &[u8], stderr: &[u8]) -> bool {
    if options.json {
        return false;
    }
    estimate_tokens_from_bytes(stdout.len() + stderr.len()) <= options.budget
}

pub fn push_budgeted_line(
    out: &mut String,
    line: &str,
    budget: usize,
    truncated: &mut bool,
) -> bool {
    let projected = estimate_tokens(out) + estimate_tokens(line) + 1;
    if projected > budget {
        *truncated = true;
        if out.is_empty() {
            out.push_str(line);
            out.push('\n');
            return true;
        }
        return false;
    }
    out.push_str(line);
    out.push('\n');
    true
}

pub fn status_footer(exit_code: i32, raw_hint: Option<&str>) -> String {
    let mut footer = format!("Exit code: {exit_code}\n");
    if let Some(hint) = raw_hint {
        footer.push_str("Raw: ");
        footer.push_str(hint);
        footer.push('\n');
    }
    footer
}
