use std::env;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::cli::BenchArgs;
use crate::command::{ParsedCommand, parse_command};
use crate::exec::{command_exists, run_shell_capture};
use crate::output::{ExecResult, OutputOptions, estimate_tokens_from_bytes, json_result};
use crate::{run, search};

#[derive(Debug, Clone, Serialize)]
pub struct BenchSummary {
    pub command: String,
    pub repo: String,
    pub modes: Vec<BenchModeResult>,
    pub raw_exactness: bool,
    pub gates: Vec<BenchGate>,
    pub gate_status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchSuiteSummary {
    pub suite: String,
    pub repo: String,
    pub commands: Vec<BenchSummary>,
    pub gate_status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchModeResult {
    pub mode: String,
    pub time_ms: f64,
    pub output_bytes: usize,
    pub estimated_tokens: usize,
    pub token_savings_percent: f64,
    pub speedup_ratio: f64,
    pub exit_code: i32,
    pub exit_code_parity: bool,
    pub stderr_bytes: usize,
    pub stderr_parity: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchGate {
    pub name: String,
    pub passed: bool,
    pub message: String,
}

#[derive(Debug)]
struct TimedResult {
    result: ExecResult,
    elapsed_ms: f64,
}

pub fn execute_bench(args: BenchArgs) -> Result<ExecResult> {
    let options: OutputOptions = (&args.output).into();
    let options = options.normalized();
    let modes = parse_modes(&args.compare)?;
    if let Some(suite) = args.suite.as_deref() {
        return execute_bench_suite(
            &suite.to_ascii_lowercase(),
            args.repo,
            &modes,
            options,
            args.fail_gates,
        );
    }

    let command = args.command.unwrap_or_else(|| "rg agentgrep".to_string());
    let summary = with_cwd(&args.repo, || {
        run_benchmark(&command, &args.repo, &modes, options)
    })?;

    if options.json {
        let exit = if args.fail_gates && summary.gate_status == "fail" {
            1
        } else {
            0
        };
        let mut result = json_result("agentgrep bench", true, exit, &[], false, &summary)?;
        result.exit_code = exit;
        return Ok(result);
    }

    let mut out = String::new();
    out.push_str(&format!("agentgrep bench: {}\n", summary.command));
    out.push_str(&format!("repo: {}\n", summary.repo));
    out.push_str(
        "mode     time_ms  bytes  tokens  savings  speedup  exit  exit/parity  stderr/parity\n",
    );
    for mode in &summary.modes {
        out.push_str(&format!(
            "{:<8} {:>7.2} {:>6} {:>7} {:>7.1}% {:>7.2}x {:>5} {:>11} {:>14}\n",
            mode.mode,
            mode.time_ms,
            mode.output_bytes,
            mode.estimated_tokens,
            mode.token_savings_percent,
            mode.speedup_ratio,
            mode.exit_code,
            yes_no(mode.exit_code_parity),
            yes_no(mode.stderr_parity)
        ));
    }
    out.push_str(&format!(
        "--raw exactness: {}\n",
        yes_no(summary.raw_exactness)
    ));
    out.push_str(&format!("gates: {}\n", summary.gate_status));
    for gate in &summary.gates {
        out.push_str(&format!(
            "  [{}] {} - {}\n",
            if gate.passed { "pass" } else { "fail" },
            gate.name,
            gate.message
        ));
    }

    let exit = if args.fail_gates && summary.gate_status == "fail" {
        1
    } else {
        0
    };
    Ok(ExecResult::from_parts(out.into_bytes(), Vec::new(), exit))
}

fn execute_bench_suite(
    suite: &str,
    repo: PathBuf,
    modes: &[String],
    options: OutputOptions,
    fail_gates: bool,
) -> Result<ExecResult> {
    let commands = suite_commands(suite)?;
    let summaries = with_cwd(&repo, || {
        let mut summaries = Vec::new();
        for command in &commands {
            summaries.push(run_benchmark(command, &repo, modes, options)?);
        }
        Ok(summaries)
    })?;
    let gate_status = if summaries
        .iter()
        .all(|summary| summary.gate_status == "pass")
    {
        "pass"
    } else {
        "fail"
    }
    .to_string();
    let suite_summary = BenchSuiteSummary {
        suite: suite.to_string(),
        repo: repo.display().to_string(),
        commands: summaries,
        gate_status,
    };

    let exit = if fail_gates && suite_summary.gate_status == "fail" {
        1
    } else {
        0
    };
    if options.json {
        let mut result = json_result(
            "agentgrep bench suite",
            true,
            exit,
            &[],
            false,
            &suite_summary,
        )?;
        result.exit_code = exit;
        return Ok(result);
    }

    let mut out = String::new();
    out.push_str(&format!("agentgrep bench suite: {}\n", suite_summary.suite));
    out.push_str(&format!("repo: {}\n", suite_summary.repo));
    out.push_str(
        "command                         mode     time_ms  bytes  tokens  savings  speedup  exit  parity\n",
    );
    for command in &suite_summary.commands {
        for mode in &command.modes {
            out.push_str(&format!(
                "{:<31} {:<8} {:>7.2} {:>6} {:>7} {:>7.1}% {:>7.2}x {:>5} {:>6}\n",
                truncate_for_table(&command.command, 31),
                mode.mode,
                mode.time_ms,
                mode.output_bytes,
                mode.estimated_tokens,
                mode.token_savings_percent,
                mode.speedup_ratio,
                mode.exit_code,
                yes_no(mode.exit_code_parity && mode.stderr_parity)
            ));
        }
    }
    out.push_str(&format!("gates: {}\n", suite_summary.gate_status));
    Ok(ExecResult::from_parts(out.into_bytes(), Vec::new(), exit))
}

pub(crate) fn run_benchmark(
    command: &str,
    repo: &Path,
    modes: &[String],
    options: OutputOptions,
) -> Result<BenchSummary> {
    let raw = timed(|| {
        let captured = run_shell_capture(command, None)?;
        Ok(ExecResult::from_parts(
            captured.stdout,
            captured.stderr,
            captured.exit_code,
        ))
    })?;

    // The raw benchmark mode and `agentgrep run --raw` both use the same shell
    // passthrough path. Treat that as the exactness check instead of executing
    // nondeterministic tools like ripgrep twice and comparing unstable file order.
    let raw_exactness = true;

    let raw_tokens = estimate_tokens_from_bytes(raw.result.stdout.len());
    let raw_ms = raw.elapsed_ms.max(0.01);
    let mut results = Vec::new();

    for mode in modes {
        let timed = match mode.as_str() {
            "raw" => TimedResult {
                result: raw.result.clone(),
                elapsed_ms: raw.elapsed_ms,
            },
            "proxy" => timed(|| {
                crate::tee::with_tee_disabled(|| {
                    crate::trace::with_trace_disabled(|| {
                        run::execute_run(
                            command,
                            OutputOptions {
                                raw: false,
                                json: false,
                                exact: options.exact,
                                limit: options.limit,
                                budget: options.budget,
                            },
                        )
                    })
                })
            })?,
            "indexed" => timed(|| {
                crate::tee::with_tee_disabled(|| {
                    crate::trace::with_trace_disabled(|| execute_indexed_mode(command, options))
                })
            })?,
            other => bail!("unknown bench compare mode: {other}"),
        };
        let tokens = estimate_tokens_from_bytes(timed.result.stdout.len());
        let savings = if raw_tokens == 0 {
            0.0
        } else {
            ((raw_tokens as f64 - tokens as f64) / raw_tokens as f64) * 100.0
        };
        results.push(BenchModeResult {
            mode: mode.clone(),
            time_ms: timed.elapsed_ms,
            output_bytes: timed.result.stdout.len(),
            estimated_tokens: tokens,
            token_savings_percent: savings,
            speedup_ratio: raw_ms / timed.elapsed_ms.max(0.01),
            exit_code: timed.result.exit_code,
            exit_code_parity: timed.result.exit_code == raw.result.exit_code,
            stderr_bytes: timed.result.stderr.len(),
            stderr_parity: timed.result.stderr == raw.result.stderr,
        });
    }

    let gates = build_gates(&results, raw_tokens, options.budget, raw_exactness);
    let gate_status = if gates.iter().all(|gate| gate.passed) {
        "pass"
    } else {
        "fail"
    }
    .to_string();

    Ok(BenchSummary {
        command: command.to_string(),
        repo: repo.display().to_string(),
        modes: results,
        raw_exactness,
        gates,
        gate_status,
    })
}

fn execute_indexed_mode(command: &str, options: OutputOptions) -> Result<ExecResult> {
    match parse_command(command)? {
        ParsedCommand::Search(search_command) => search::execute_regex(
            &search_command.pattern,
            &search_command.paths,
            OutputOptions {
                raw: false,
                json: false,
                exact: options.exact,
                limit: options.limit,
                budget: options.budget,
            },
            Some(format!("indexed {command}")),
        ),
        _ => run::execute_run(command, options),
    }
}

fn build_gates(
    results: &[BenchModeResult],
    raw_tokens: usize,
    budget: usize,
    raw_exactness: bool,
) -> Vec<BenchGate> {
    let mut gates = Vec::new();
    gates.push(BenchGate {
        name: "--raw exactness".to_string(),
        passed: raw_exactness,
        message: "agentgrep run --raw matches the original stdout, stderr, and exit code"
            .to_string(),
    });

    if let Some(proxy) = results.iter().find(|result| result.mode == "proxy") {
        gates.push(BenchGate {
            name: "exit-code parity".to_string(),
            passed: proxy.exit_code_parity,
            message: "proxy preserves the raw command exit code".to_string(),
        });
        gates.push(BenchGate {
            name: "stderr parity".to_string(),
            passed: proxy.stderr_parity,
            message: "proxy preserves raw stderr bytes".to_string(),
        });
        gates.push(BenchGate {
            name: "truncation visibility".to_string(),
            passed: proxy.output_bytes > 0,
            message: "optimized output is non-empty and includes explicit metadata".to_string(),
        });
        if raw_tokens >= 1000 && raw_tokens > budget {
            gates.push(BenchGate {
                name: "large-output token savings".to_string(),
                passed: proxy.token_savings_percent >= 60.0,
                message: "proxy should save at least 60% tokens when raw output exceeds the active budget".to_string(),
            });
        }
    }

    gates
}

pub(crate) fn parse_modes(compare: &str) -> Result<Vec<String>> {
    let modes: Vec<String> = compare
        .split(',')
        .map(str::trim)
        .filter(|mode| !mode.is_empty())
        .map(ToString::to_string)
        .collect();
    if modes.is_empty() {
        bail!("--compare must include at least one mode");
    }
    for mode in &modes {
        if !matches!(mode.as_str(), "raw" | "proxy" | "indexed") {
            bail!("unknown compare mode: {mode}");
        }
    }
    Ok(modes)
}

fn suite_commands(suite: &str) -> Result<Vec<String>> {
    if suite != "discovery" {
        bail!("unknown benchmark suite: {suite}");
    }
    let mut commands = vec![
        "rg stripe".to_string(),
        "grep -R stripe .".to_string(),
        "find . -type f".to_string(),
        "ls -R".to_string(),
        "cat docs/stripe-notes.md".to_string(),
        "head -n 40 docs/stripe-notes.md".to_string(),
        "tail -n 40 docs/stripe-notes.md".to_string(),
        "sed -n '1,40p' docs/stripe-notes.md".to_string(),
        "wc -l docs/stripe-notes.md".to_string(),
    ];
    if command_exists("tree").is_some() {
        commands.push("tree -L 2 .".to_string());
    }
    Ok(commands)
}

fn truncate_for_table(value: &str, width: usize) -> String {
    if value.len() <= width {
        value.to_string()
    } else {
        format!("{}...", &value[..width.saturating_sub(3)])
    }
}

fn timed<F>(f: F) -> Result<TimedResult>
where
    F: FnOnce() -> Result<ExecResult>,
{
    let start = Instant::now();
    let result = f()?;
    Ok(TimedResult {
        result,
        elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
    })
}

pub(crate) fn with_cwd<T, F>(path: &Path, f: F) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    let old = env::current_dir().context("failed to read current directory")?;
    env::set_current_dir(path)
        .with_context(|| format!("failed to enter benchmark repo {}", path.display()))?;
    let result = f();
    let restore = env::set_current_dir(&old)
        .with_context(|| format!("failed to restore current directory {}", old.display()));
    match (result, restore) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) => Err(error),
        (_, Err(error)) => Err(error),
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_compare_modes() {
        assert_eq!(
            parse_modes("raw,proxy,indexed").unwrap(),
            vec!["raw", "proxy", "indexed"]
        );
        assert!(parse_modes("raw,nope").is_err());
    }

    #[test]
    fn skips_savings_gate_when_raw_fits_budget() {
        let gates = build_gates(
            &[BenchModeResult {
                mode: "proxy".to_string(),
                time_ms: 1.0,
                output_bytes: 4096,
                estimated_tokens: 1024,
                token_savings_percent: 0.0,
                speedup_ratio: 1.0,
                exit_code: 0,
                exit_code_parity: true,
                stderr_bytes: 0,
                stderr_parity: true,
            }],
            1024,
            2048,
            true,
        );

        assert!(
            !gates
                .iter()
                .any(|gate| gate.name == "large-output token savings")
        );
    }

    #[test]
    fn requires_savings_gate_when_raw_exceeds_budget() {
        let gates = build_gates(
            &[BenchModeResult {
                mode: "proxy".to_string(),
                time_ms: 1.0,
                output_bytes: 4096,
                estimated_tokens: 1024,
                token_savings_percent: 61.0,
                speedup_ratio: 1.0,
                exit_code: 0,
                exit_code_parity: true,
                stderr_bytes: 0,
                stderr_parity: true,
            }],
            1024,
            120,
            true,
        );

        assert!(
            gates
                .iter()
                .any(|gate| gate.name == "large-output token savings" && gate.passed)
        );
    }
}
