use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::bench;
use crate::cli::{
    TraceArgs, TraceCommands, TraceImportClaudeArgs, TraceImportCodexArgs, TraceReplayArgs,
    TraceSummaryArgs,
};
use crate::command::{FileSliceKind, GitCommand, ParsedCommand, SearchKind, parse_command};
use crate::output::{
    ExecResult, OutputOptions, estimate_tokens_from_bytes, json_result, push_budgeted_line,
};

const TRACE_VERSION: u32 = 1;
static TRACE_DISABLED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceRecord {
    pub version: u32,
    pub source: String,
    pub ts: i64,
    pub cwd: Option<String>,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_command: Option<String>,
    pub family: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
struct TraceImportSummary {
    db: String,
    out: String,
    scanned_rows: usize,
    imported_records: usize,
    unique_commands: usize,
    cwd_filter: Option<String>,
    thread_filter: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct TraceImportClaudeSummary {
    dir: String,
    out: String,
    scanned_rows: usize,
    imported_records: usize,
    unique_commands: usize,
    cwd_filter: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct TraceSummary {
    path: String,
    records: usize,
    unique_commands: usize,
    families: Vec<TraceCount>,
    commands: Vec<TraceCount>,
    workdirs: Vec<TraceCount>,
}

#[derive(Debug, Clone, Serialize)]
struct TraceReplaySummary {
    path: String,
    repo: String,
    commands: Vec<TraceReplayCommand>,
    skipped: Vec<SkippedTraceCommand>,
    gate_status: String,
}

#[derive(Debug, Clone, Serialize)]
struct TraceReplayCommand {
    command: String,
    modes: Vec<crate::bench::BenchModeResult>,
    gate_status: String,
}

#[derive(Debug, Clone, Serialize)]
struct TraceCount {
    value: String,
    count: usize,
}

#[derive(Debug, Clone, Serialize)]
struct SkippedTraceCommand {
    command: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
struct SqliteLogRow {
    ts: i64,
    thread_id: Option<String>,
    feedback_log_body: Option<String>,
}

#[derive(Debug)]
struct CodexExecCall {
    ts: i64,
    thread_id: Option<String>,
    call_id: Option<String>,
    command: String,
    workdir: Option<String>,
}

#[derive(Debug)]
struct ClaudeBashCall {
    ts: i64,
    command: String,
    workdir: Option<String>,
}

#[derive(Debug, Default)]
struct PendingCodexArgs {
    ts: i64,
    thread_id: Option<String>,
    arguments: String,
}

#[derive(Debug)]
enum CodexStreamEvent {
    ArgumentsDelta { key: String, delta: String },
    OutputItemDone { key: String, item: Value },
}

pub fn execute_trace(args: TraceArgs) -> Result<ExecResult> {
    match args.command {
        TraceCommands::ImportCodex(args) => execute_import_codex(args),
        TraceCommands::ImportClaude(args) => execute_import_claude(args),
        TraceCommands::Summary(args) => execute_summary(args),
        TraceCommands::Replay(args) => execute_replay(args),
    }
}

pub fn resolve_trace_path(explicit: Option<PathBuf>) -> Option<PathBuf> {
    if TRACE_DISABLED.load(Ordering::SeqCst) {
        return None;
    }
    if let Some(path) = explicit {
        return Some(path);
    }
    match env::var("AGENTGREP_TRACE") {
        Ok(value) if value == "1" || value.eq_ignore_ascii_case("true") => {
            Some(default_trace_path("commands.jsonl"))
        }
        Ok(value)
            if value == "0"
                || value.is_empty()
                || value.eq_ignore_ascii_case("false")
                || value.eq_ignore_ascii_case("off") =>
        {
            None
        }
        Ok(value) => Some(PathBuf::from(value)),
        Err(_) => None,
    }
}

pub fn append_run_record(
    path: &Path,
    command: &str,
    result: &ExecResult,
    elapsed_ms: f64,
) -> Result<()> {
    let cwd = env::current_dir()
        .ok()
        .map(|path| path.display().to_string());
    let record = TraceRecord {
        version: TRACE_VERSION,
        source: "agentgrep-run".to_string(),
        ts: now_epoch_seconds(),
        cwd,
        command: command.to_string(),
        observed_command: None,
        family: command_family(command),
        exit_code: Some(result.exit_code),
        stdout_bytes: Some(result.stdout.len()),
        stderr_bytes: Some(result.stderr.len()),
        estimated_tokens: Some(estimate_tokens_from_bytes(
            result.stdout.len() + result.stderr.len(),
        )),
        elapsed_ms: Some(elapsed_ms),
    };
    append_record(path, &record)
}

pub fn with_trace_disabled<T>(f: impl FnOnce() -> T) -> T {
    let _guard = TraceDisableGuard {
        previous: TRACE_DISABLED.swap(true, Ordering::SeqCst),
    };
    f()
}

struct TraceDisableGuard {
    previous: bool,
}

impl Drop for TraceDisableGuard {
    fn drop(&mut self) {
        TRACE_DISABLED.store(self.previous, Ordering::SeqCst);
    }
}

fn execute_import_codex(args: TraceImportCodexArgs) -> Result<ExecResult> {
    let options: OutputOptions = (&args.output).into();
    let options = options.normalized();
    let db = expand_home(&args.db);
    let cwd_filter = args
        .cwd
        .unwrap_or(env::current_dir().context("failed to read current directory")?);
    let calls = import_codex_calls(&db, cwd_filter.as_path(), args.thread.as_deref(), args.rows)?;

    let mut records = Vec::new();
    let mut unique = HashSet::new();
    for call in &calls {
        let (command, observed_command) = normalize_observed_command(&call.command);
        unique.insert(command.clone());
        records.push(TraceRecord {
            version: TRACE_VERSION,
            source: "codex-sqlite".to_string(),
            ts: call.ts,
            cwd: call.workdir.clone(),
            command: command.clone(),
            observed_command,
            family: command_family(&command),
            exit_code: None,
            stdout_bytes: None,
            stderr_bytes: None,
            estimated_tokens: None,
            elapsed_ms: None,
        });
    }

    let out_path = expand_home_path(&args.out);
    write_records(&out_path, &records)?;
    let summary = TraceImportSummary {
        db: db.display().to_string(),
        out: out_path.display().to_string(),
        scanned_rows: args.rows,
        imported_records: records.len(),
        unique_commands: unique.len(),
        cwd_filter: Some(cwd_filter.display().to_string()),
        thread_filter: args.thread,
    };

    if options.json {
        return json_result(
            "agentgrep trace import-codex",
            true,
            0,
            &[],
            false,
            &summary,
        );
    }

    let mut out = String::new();
    let mut truncated = false;
    push_budgeted_line(
        &mut out,
        &format!("agentgrep trace import-codex: {}", summary.out),
        options.budget,
        &mut truncated,
    );
    out.push_str(&format!(
        "Imported {} records, {} unique commands from {}.\n",
        summary.imported_records, summary.unique_commands, summary.db
    ));
    out.push_str("Trace stores command metadata only; stdout/stderr content is not captured.\n");
    Ok(ExecResult::from_parts(out.into_bytes(), Vec::new(), 0))
}

fn execute_import_claude(args: TraceImportClaudeArgs) -> Result<ExecResult> {
    let options: OutputOptions = (&args.output).into();
    let options = options.normalized();
    let dir = expand_home(&args.dir);
    let cwd_filter = args
        .cwd
        .unwrap_or(env::current_dir().context("failed to read current directory")?);
    let (calls, scanned_rows) = import_claude_calls(&dir, cwd_filter.as_path(), args.rows)?;

    let mut records = Vec::new();
    let mut unique = HashSet::new();
    for call in &calls {
        unique.insert(call.command.clone());
        records.push(TraceRecord {
            version: TRACE_VERSION,
            source: "claude-jsonl".to_string(),
            ts: call.ts,
            cwd: call.workdir.clone(),
            command: call.command.clone(),
            observed_command: None,
            family: command_family(&call.command),
            exit_code: None,
            stdout_bytes: None,
            stderr_bytes: None,
            estimated_tokens: None,
            elapsed_ms: None,
        });
    }

    let out_path = expand_home_path(&args.out);
    write_records(&out_path, &records)?;
    let summary = TraceImportClaudeSummary {
        dir: dir.display().to_string(),
        out: out_path.display().to_string(),
        scanned_rows,
        imported_records: records.len(),
        unique_commands: unique.len(),
        cwd_filter: Some(cwd_filter.display().to_string()),
    };

    if options.json {
        return json_result(
            "agentgrep trace import-claude",
            true,
            0,
            &[],
            false,
            &summary,
        );
    }

    let mut out = String::new();
    let mut truncated = false;
    push_budgeted_line(
        &mut out,
        &format!("agentgrep trace import-claude: {}", summary.out),
        options.budget,
        &mut truncated,
    );
    out.push_str(&format!(
        "Imported {} records, {} unique commands from {}.\n",
        summary.imported_records, summary.unique_commands, summary.dir
    ));
    out.push_str("Trace stores command metadata only; stdout/stderr content is not captured.\n");
    Ok(ExecResult::from_parts(out.into_bytes(), Vec::new(), 0))
}

fn execute_summary(args: TraceSummaryArgs) -> Result<ExecResult> {
    let options: OutputOptions = (&args.output).into();
    let options = options.normalized();
    let path = expand_home_path(&args.path);
    let records = load_records(&path)?;
    let summary = summarize_records(&path, &records, options.limit);

    if options.json {
        return json_result("agentgrep trace summary", true, 0, &[], false, &summary);
    }

    let mut out = String::new();
    let mut truncated = false;
    push_budgeted_line(
        &mut out,
        &format!("agentgrep trace summary: {}", summary.path),
        options.budget,
        &mut truncated,
    );
    push_budgeted_line(
        &mut out,
        &format!(
            "{} records, {} unique commands.",
            summary.records, summary.unique_commands
        ),
        options.budget,
        &mut truncated,
    );
    render_counts(
        &mut out,
        "Families",
        &summary.families,
        options,
        &mut truncated,
    );
    render_counts(
        &mut out,
        "Top commands",
        &summary.commands,
        options,
        &mut truncated,
    );
    render_counts(
        &mut out,
        "Workdirs",
        &summary.workdirs,
        options,
        &mut truncated,
    );
    if truncated {
        out.push_str(
            "Truncated: use --limit, --budget, or --json for more trace summary output.\n",
        );
    }
    Ok(ExecResult::from_parts(out.into_bytes(), Vec::new(), 0))
}

fn execute_replay(args: TraceReplayArgs) -> Result<ExecResult> {
    let options: OutputOptions = (&args.output).into();
    let options = options.normalized();
    let modes = bench::parse_modes(&args.compare)?;
    let path = expand_home_path(&args.path);
    let records = load_records(&path)?;
    let mut seen = HashSet::new();
    let mut commands = Vec::new();
    let mut skipped = Vec::new();

    for record in records {
        if !seen.insert(record.command.clone()) {
            continue;
        }
        match replay_safety(&record.command) {
            Ok(()) => {
                if commands.len() < args.commands {
                    commands.push(record.command);
                }
            }
            Err(reason) => skipped.push(SkippedTraceCommand {
                command: record.command,
                reason,
            }),
        }
    }

    let summaries = bench::with_cwd(&args.repo, || {
        let mut summaries = Vec::new();
        for command in &commands {
            summaries.push(bench::run_benchmark(command, &args.repo, &modes, options)?);
        }
        Ok(summaries)
    })?;

    let command_summaries: Vec<TraceReplayCommand> = summaries
        .into_iter()
        .map(|summary| TraceReplayCommand {
            command: summary.command,
            modes: summary.modes,
            gate_status: summary.gate_status,
        })
        .collect();
    let gate_status = if command_summaries
        .iter()
        .all(|summary| summary.gate_status == "pass")
    {
        "pass"
    } else {
        "fail"
    }
    .to_string();
    let replay = TraceReplaySummary {
        path: path.display().to_string(),
        repo: args.repo.display().to_string(),
        commands: command_summaries,
        skipped,
        gate_status,
    };

    let exit = if args.fail_gates && replay.gate_status == "fail" {
        1
    } else {
        0
    };
    if options.json {
        let mut result = json_result("agentgrep trace replay", true, exit, &[], false, &replay)?;
        result.exit_code = exit;
        return Ok(result);
    }

    let mut out = String::new();
    out.push_str(&format!("agentgrep trace replay: {}\n", replay.path));
    out.push_str(&format!("repo: {}\n", replay.repo));
    out.push_str(
        "command                         mode     time_ms  bytes  tokens  savings  speedup  exit  parity\n",
    );
    for command in &replay.commands {
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
    out.push_str(&format!(
        "skipped unsafe/unsupported commands: {}\n",
        replay.skipped.len()
    ));
    for skipped in replay.skipped.iter().take(options.limit) {
        out.push_str(&format!("  - {} ({})\n", skipped.command, skipped.reason));
    }
    out.push_str(&format!("gates: {}\n", replay.gate_status));
    Ok(ExecResult::from_parts(out.into_bytes(), Vec::new(), exit))
}

fn import_codex_calls(
    db: &Path,
    cwd_filter: &Path,
    thread_filter: Option<&str>,
    rows: usize,
) -> Result<Vec<CodexExecCall>> {
    if !db.exists() {
        bail!("Codex log database not found: {}", db.display());
    }
    let mut sql = "select ts, thread_id, feedback_log_body from logs".to_string();
    if let Some(thread) = thread_filter {
        sql.push_str(" where thread_id = '");
        sql.push_str(&thread.replace('\'', "''"));
        sql.push('\'');
    }
    sql.push_str(" order by ts desc, ts_nanos desc limit ");
    sql.push_str(&rows.to_string());

    let output = Command::new("sqlite3")
        .args(["-readonly", "-json"])
        .arg(db)
        .arg(sql)
        .output()
        .context("failed to run sqlite3 while importing Codex logs")?;
    if !output.status.success() {
        bail!(
            "sqlite3 failed while importing Codex logs: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let mut rows: Vec<SqliteLogRow> =
        serde_json::from_slice(&output.stdout).context("failed to parse sqlite3 JSON output")?;
    let mut calls = Vec::new();
    let mut seen = HashSet::new();
    let mut pending_args = HashMap::<String, PendingCodexArgs>::new();
    rows.reverse();
    for row in rows {
        let Some(body) = row.feedback_log_body else {
            continue;
        };
        if let Some(event) = parse_codex_stream_event(row.ts, row.thread_id.clone(), &body) {
            match event {
                CodexStreamEvent::ArgumentsDelta { key, delta } => {
                    let pending = pending_args.entry(key).or_insert_with(|| PendingCodexArgs {
                        ts: row.ts,
                        thread_id: row.thread_id.clone(),
                        arguments: String::new(),
                    });
                    pending.ts = row.ts;
                    pending.arguments.push_str(&delta);
                }
                CodexStreamEvent::OutputItemDone { key, item } => {
                    if item.get("name").and_then(Value::as_str) == Some("exec_command")
                        && item
                            .get("arguments")
                            .and_then(Value::as_str)
                            .is_none_or(str::is_empty)
                        && let Some(pending) = pending_args.remove(&key)
                        && let Some(call) = codex_call_from_args_text(
                            row.ts,
                            row.thread_id.clone().or(pending.thread_id),
                            item.get("call_id")
                                .and_then(Value::as_str)
                                .map(ToString::to_string),
                            &pending.arguments,
                        )
                    {
                        push_codex_call_if_in_scope(
                            &mut calls,
                            &mut seen,
                            call,
                            cwd_filter,
                            thread_filter,
                        );
                    }
                }
            }
        }
        for call in parse_codex_exec_bodies(row.ts, row.thread_id.clone(), &body) {
            push_codex_call_if_in_scope(&mut calls, &mut seen, call, cwd_filter, thread_filter);
        }
    }
    Ok(calls)
}

fn push_codex_call_if_in_scope(
    calls: &mut Vec<CodexExecCall>,
    seen: &mut HashSet<String>,
    mut call: CodexExecCall,
    cwd_filter: &Path,
    thread_filter: Option<&str>,
) {
    let Some(workdir) = &call.workdir else {
        return;
    };
    if !path_starts_with(Path::new(workdir), cwd_filter) {
        return;
    }
    let key = call
        .call_id
        .clone()
        .unwrap_or_else(|| format!("{}:{}", call.ts, call.command));
    if !seen.insert(key) {
        return;
    }
    if let Some(thread) = thread_filter {
        call.thread_id = Some(thread.to_string());
    }
    calls.push(call);
}

fn import_claude_calls(
    dir: &Path,
    cwd_filter: &Path,
    rows: usize,
) -> Result<(Vec<ClaudeBashCall>, usize)> {
    if !dir.exists() {
        bail!("Claude projects directory not found: {}", dir.display());
    }
    let mut files = collect_jsonl_files(dir)?;
    files.sort_by(|left, right| {
        modified_seconds(right)
            .cmp(&modified_seconds(left))
            .then_with(|| left.cmp(right))
    });

    let mut calls = Vec::new();
    let mut scanned = 0usize;
    for file in files {
        let content = fs::read_to_string(&file)
            .with_context(|| format!("failed to read Claude log {}", file.display()))?;
        for line in content.lines() {
            if scanned >= rows {
                return Ok((calls, scanned));
            }
            scanned += 1;
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let Some(workdir) = value
                .get("cwd")
                .and_then(Value::as_str)
                .map(ToString::to_string)
            else {
                continue;
            };
            if !path_starts_with(Path::new(&workdir), cwd_filter) {
                continue;
            }
            let ts = now_epoch_seconds();
            collect_claude_bash_calls(&value, ts, Some(&workdir), &mut calls);
        }
    }
    Ok((calls, scanned))
}

fn collect_jsonl_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            files.extend(collect_jsonl_files(&path)?);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
    Ok(files)
}

fn modified_seconds(path: &Path) -> u64 {
    path.metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn collect_claude_bash_calls(
    value: &Value,
    ts: i64,
    workdir: Option<&str>,
    calls: &mut Vec<ClaudeBashCall>,
) {
    match value {
        Value::Object(map) => {
            if map.get("type").and_then(Value::as_str) == Some("tool_use")
                && map.get("name").and_then(Value::as_str) == Some("Bash")
                && let Some(command) = map
                    .get("input")
                    .and_then(|input| input.get("command"))
                    .and_then(Value::as_str)
            {
                calls.push(ClaudeBashCall {
                    ts,
                    command: command.to_string(),
                    workdir: workdir.map(ToString::to_string),
                });
                return;
            }
            for nested in map.values() {
                collect_claude_bash_calls(nested, ts, workdir, calls);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_claude_bash_calls(item, ts, workdir, calls);
            }
        }
        _ => {}
    }
}

fn parse_codex_exec_bodies(ts: i64, thread_id: Option<String>, body: &str) -> Vec<CodexExecCall> {
    let mut calls = Vec::new();
    if let Some(call) = parse_codex_tool_call_body(ts, thread_id.clone(), body) {
        calls.push(call);
    }
    let Some(payload) = codex_json_payload(body) else {
        return calls;
    };
    let Ok(value) = serde_json::from_str::<Value>(payload) else {
        return calls;
    };
    if let Some(item) = value.get("item")
        && let Some(call) = parse_codex_exec_item(ts, thread_id.clone(), item)
    {
        calls.push(call);
    }
    if let Some(output) = value
        .get("response")
        .and_then(|response| response.get("output"))
        .and_then(Value::as_array)
    {
        for item in output {
            if let Some(call) = parse_codex_exec_item(ts, thread_id.clone(), item) {
                calls.push(call);
            }
        }
    }
    calls
}

fn parse_codex_exec_item(
    ts: i64,
    thread_id: Option<String>,
    item: &Value,
) -> Option<CodexExecCall> {
    if item.get("name")?.as_str()? != "exec_command" {
        return None;
    }
    let args_text = item.get("arguments")?.as_str()?;
    if args_text.is_empty() {
        return None;
    }
    let call_id = item
        .get("call_id")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    codex_call_from_args_text(ts, thread_id, call_id, args_text)
}

fn codex_call_from_args_text(
    ts: i64,
    thread_id: Option<String>,
    call_id: Option<String>,
    args_text: &str,
) -> Option<CodexExecCall> {
    let args: Value = serde_json::from_str(args_text).ok()?;
    let command = args.get("cmd")?.as_str()?.to_string();
    let workdir = args
        .get("workdir")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    Some(CodexExecCall {
        ts,
        thread_id,
        call_id,
        command,
        workdir,
    })
}

fn parse_codex_stream_event(
    _ts: i64,
    thread_id: Option<String>,
    body: &str,
) -> Option<CodexStreamEvent> {
    let payload = codex_json_payload(body)?;
    let value = serde_json::from_str::<Value>(payload).ok()?;
    match value.get("type").and_then(Value::as_str)? {
        "response.function_call_arguments.delta" => Some(CodexStreamEvent::ArgumentsDelta {
            key: codex_stream_key(thread_id.as_deref(), &value, None)?,
            delta: value.get("delta")?.as_str()?.to_string(),
        }),
        "response.output_item.done" => {
            let item = value.get("item")?.clone();
            Some(CodexStreamEvent::OutputItemDone {
                key: codex_stream_key(thread_id.as_deref(), &value, Some(&item))?,
                item,
            })
        }
        _ => None,
    }
}

fn codex_stream_key(
    thread_id: Option<&str>,
    event: &Value,
    item: Option<&Value>,
) -> Option<String> {
    if let Some(id) = event
        .get("item_id")
        .or_else(|| item.and_then(|item| item.get("id")))
        .and_then(Value::as_str)
    {
        return Some(format!("{}:{id}", thread_id.unwrap_or_default()));
    }
    event
        .get("output_index")
        .and_then(Value::as_i64)
        .map(|index| format!("{}:output:{index}", thread_id.unwrap_or_default()))
}

fn parse_codex_tool_call_body(
    ts: i64,
    thread_id: Option<String>,
    body: &str,
) -> Option<CodexExecCall> {
    let marker = "ToolCall: exec_command ";
    let idx = body.find(marker)?;
    let json = json_object_prefix(body[idx + marker.len()..].trim())?;
    let args: Value = serde_json::from_str(json).ok()?;
    let command = args.get("cmd")?.as_str()?.to_string();
    let workdir = args
        .get("workdir")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    Some(CodexExecCall {
        ts,
        thread_id,
        call_id: None,
        command,
        workdir,
    })
}

fn json_object_prefix(input: &str) -> Option<&str> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut start = None;
    for (idx, ch) in input.char_indices() {
        if start.is_none() {
            if ch == '{' {
                start = Some(idx);
                depth = 1;
            }
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let start_idx = start?;
                    return Some(&input[start_idx..=idx]);
                }
            }
            _ => {}
        }
    }
    None
}

fn codex_json_payload(body: &str) -> Option<&str> {
    for marker in ["Received message ", "websocket event: "] {
        if let Some(idx) = body.find(marker) {
            return Some(body[idx + marker.len()..].trim());
        }
    }
    let json_start = body.find('{')?;
    Some(body[json_start..].trim())
}

fn append_record(path: &Path, record: &TraceRecord) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create trace dir {}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open trace {}", path.display()))?;
    serde_json::to_writer(&mut file, record)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn write_records(path: &Path, records: &[TraceRecord]) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create trace dir {}", parent.display()))?;
    }
    let mut file = fs::File::create(path)
        .with_context(|| format!("failed to create trace {}", path.display()))?;
    for record in records {
        serde_json::to_writer(&mut file, record)?;
        file.write_all(b"\n")?;
    }
    Ok(())
}

fn load_records(path: &Path) -> Result<Vec<TraceRecord>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read trace {}", path.display()))?;
    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).context("failed to parse trace JSONL record"))
        .collect()
}

fn summarize_records(path: &Path, records: &[TraceRecord], limit: usize) -> TraceSummary {
    let mut families = BTreeMap::new();
    let mut commands = BTreeMap::new();
    let mut workdirs = BTreeMap::new();
    for record in records {
        *families.entry(record.family.clone()).or_insert(0) += 1;
        *commands.entry(record.command.clone()).or_insert(0) += 1;
        if let Some(cwd) = &record.cwd {
            *workdirs.entry(cwd.clone()).or_insert(0) += 1;
        }
    }
    TraceSummary {
        path: path.display().to_string(),
        records: records.len(),
        unique_commands: commands.len(),
        families: top_counts(families, limit),
        commands: top_counts(commands, limit),
        workdirs: top_counts(workdirs, limit),
    }
}

fn top_counts(counts: BTreeMap<String, usize>, limit: usize) -> Vec<TraceCount> {
    let mut counts: Vec<TraceCount> = counts
        .into_iter()
        .map(|(value, count)| TraceCount { value, count })
        .collect();
    counts.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value.cmp(&b.value)));
    counts.truncate(limit);
    counts
}

fn render_counts(
    out: &mut String,
    label: &str,
    counts: &[TraceCount],
    options: OutputOptions,
    truncated: &mut bool,
) {
    if counts.is_empty() {
        return;
    }
    push_budgeted_line(out, &format!("{label}:"), options.budget, truncated);
    for count in counts {
        if !push_budgeted_line(
            out,
            &format!("  {:>4} {}", count.count, count.value),
            options.budget,
            truncated,
        ) {
            break;
        }
    }
}

fn replay_safety(command: &str) -> std::result::Result<(), String> {
    let parsed = parse_command(command).map_err(|error| error.to_string())?;
    if has_unsafe_shell_syntax(command, &parsed) {
        return Err("contains shell control or redirection syntax".to_string());
    }
    match parsed {
        ParsedCommand::Search(_)
        | ParsedCommand::FindMap { .. }
        | ParsedCommand::LsRecursive { .. }
        | ParsedCommand::TreeMap { .. }
        | ParsedCommand::Cat { .. }
        | ParsedCommand::FileSlice(_)
        | ParsedCommand::WcLines { .. }
        | ParsedCommand::Test(_)
        | ParsedCommand::Deps { .. }
        | ParsedCommand::Git(GitCommand::ReadOnly { .. }) => Ok(()),
        ParsedCommand::Git(GitCommand::Mutating { .. }) => Err("mutating git command".to_string()),
        ParsedCommand::Unsupported { reason } => Err(reason),
    }
}

fn has_unsafe_shell_syntax(command: &str, parsed: &ParsedCommand) -> bool {
    if command.contains("$(")
        || command.contains('`')
        || command.contains("&&")
        || command.contains("||")
        || command.contains(';')
        || command.contains('>')
        || command.contains('<')
        || command.contains('\n')
    {
        return true;
    }
    if command.contains('|') {
        return !matches!(
            parsed,
            ParsedCommand::FileSlice(slice) if slice.kind == FileSliceKind::NumberedSed
        );
    }
    false
}

fn command_family(command: &str) -> String {
    match parse_command(command) {
        Ok(ParsedCommand::Search(search)) => match search.kind {
            SearchKind::Rg => "rg".to_string(),
            SearchKind::Grep => "grep".to_string(),
            SearchKind::GitGrep => "git grep".to_string(),
        },
        Ok(ParsedCommand::FindMap { .. }) => "find".to_string(),
        Ok(ParsedCommand::LsRecursive { .. }) => "ls -R".to_string(),
        Ok(ParsedCommand::TreeMap { .. }) => "tree".to_string(),
        Ok(ParsedCommand::Cat { .. }) => "cat".to_string(),
        Ok(ParsedCommand::FileSlice(slice)) => match slice.kind {
            FileSliceKind::Head => "head".to_string(),
            FileSliceKind::Tail => "tail".to_string(),
            FileSliceKind::Sed => "sed".to_string(),
            FileSliceKind::NumberedSed => "nl|sed".to_string(),
        },
        Ok(ParsedCommand::WcLines { .. }) => "wc -l".to_string(),
        Ok(ParsedCommand::Test(runner)) => match runner {
            crate::command::TestCommand::CargoTest => "cargo test".to_string(),
            crate::command::TestCommand::CargoCheck => "cargo check".to_string(),
            crate::command::TestCommand::CargoClippy => "cargo clippy".to_string(),
            crate::command::TestCommand::Pytest => "pytest".to_string(),
            crate::command::TestCommand::GoTest => "go test".to_string(),
            crate::command::TestCommand::Npm => "npm".to_string(),
            crate::command::TestCommand::Pnpm => "pnpm".to_string(),
            crate::command::TestCommand::Yarn => "yarn".to_string(),
            crate::command::TestCommand::Vitest => "vitest".to_string(),
            crate::command::TestCommand::Jest => "jest".to_string(),
            crate::command::TestCommand::Playwright => "playwright".to_string(),
            crate::command::TestCommand::Ruff => "ruff".to_string(),
            crate::command::TestCommand::Mypy => "mypy".to_string(),
        },
        Ok(ParsedCommand::Deps { .. }) => "deps".to_string(),
        Ok(ParsedCommand::Git(GitCommand::ReadOnly { subcommand, .. })) => {
            format!("git {}", subcommand.as_str())
        }
        Ok(ParsedCommand::Git(GitCommand::Mutating { .. })) => "git mutating".to_string(),
        Ok(ParsedCommand::Unsupported { .. }) => "unsupported".to_string(),
        Err(_) => "parse_error".to_string(),
    }
}

fn normalize_observed_command(command: &str) -> (String, Option<String>) {
    let Ok(words) = shell_words::split(command) else {
        return (command.to_string(), None);
    };
    if words.len() < 3 {
        return (command.to_string(), None);
    }
    let executable = Path::new(&words[0])
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&words[0]);
    if executable != "agentgrep" || words[1] != "run" {
        return (command.to_string(), None);
    }
    if words[3..]
        .iter()
        .any(|word| matches!(word.as_str(), "&&" | "||" | ";" | "|"))
    {
        return (command.to_string(), None);
    }
    (words[2].clone(), Some(command.to_string()))
}

fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(path)
}

fn expand_home_path(path: &Path) -> PathBuf {
    expand_home(&path.display().to_string())
}

fn default_trace_path(file_name: &str) -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".agentgrep/traces").join(file_name))
        .unwrap_or_else(|| PathBuf::from(".agentgrep/traces").join(file_name))
}

fn path_starts_with(path: &Path, prefix: &Path) -> bool {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let prefix = prefix
        .canonicalize()
        .unwrap_or_else(|_| prefix.to_path_buf());
    path.starts_with(prefix)
}

fn now_epoch_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

fn truncate_for_table(value: &str, width: usize) -> String {
    if value.len() <= width {
        value.to_string()
    } else {
        format!("{}...", &value[..width.saturating_sub(3)])
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_agentgrep_run_commands_from_codex_logs() {
        let (command, observed) =
            normalize_observed_command("./target/release/agentgrep run \"rg stripe\" --budget 120");
        assert_eq!(command, "rg stripe");
        assert!(observed.is_some());
    }

    #[test]
    fn does_not_unwrap_composite_agentgrep_commands() {
        let (command, observed) = normalize_observed_command(
            "./target/release/agentgrep run \"git status\" && git commit -m x",
        );
        assert_eq!(
            command,
            "./target/release/agentgrep run \"git status\" && git commit -m x"
        );
        assert!(observed.is_none());
    }

    #[test]
    fn replay_rejects_mutating_or_composite_commands() {
        assert!(replay_safety("git status").is_ok());
        assert!(replay_safety("git commit -m nope").is_err());
        assert!(replay_safety("git status && git commit -m nope").is_err());
        assert!(replay_safety("rg stripe > out.txt").is_err());
    }

    #[test]
    fn parses_codex_exec_output_item() {
        let body = r#"session_loop{thread_id=abc}:websocket event: {"type":"response.output_item.done","item":{"type":"function_call","status":"completed","arguments":"{\"cmd\":\"./target/release/agentgrep run \\\"rg stripe\\\" --budget 120\",\"workdir\":\"/tmp/repo\"}","call_id":"call_1","name":"exec_command"}}"#;
        let call = parse_codex_exec_bodies(10, Some("thread".to_string()), body)
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(
            call.command,
            "./target/release/agentgrep run \"rg stripe\" --budget 120"
        );
        assert_eq!(call.workdir.as_deref(), Some("/tmp/repo"));
        assert_eq!(call.call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn parses_codex_tool_call_body() {
        let body = r#"session_loop{thread_id=abc}:handle_output_item_done: ToolCall: exec_command {"cmd":"./target/release/agentgrep run \"git status\" --limit 80","workdir":"/tmp/repo","yield_time_ms":10000} duration_ms=12"#;
        let call = parse_codex_exec_bodies(10, Some("thread".to_string()), body)
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(
            call.command,
            "./target/release/agentgrep run \"git status\" --limit 80"
        );
        assert_eq!(call.workdir.as_deref(), Some("/tmp/repo"));
    }

    #[test]
    fn parses_codex_response_completed_output_array() {
        let body = r#"Received message {"type":"response.completed","response":{"output":[{"type":"function_call","arguments":"{\"cmd\":\"./target/release/agentgrep run \\\"git status\\\" --limit 80\",\"workdir\":\"/tmp/repo\"}","call_id":"call_1","name":"exec_command"},{"type":"message","content":[]}]} }"#;
        let calls = parse_codex_exec_bodies(10, Some("thread".to_string()), body);
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].command,
            "./target/release/agentgrep run \"git status\" --limit 80"
        );
    }
}
