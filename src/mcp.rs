use std::cell::RefCell;
use std::io::{self, BufRead, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::cli::McpArgs;
use crate::code_intel::{
    CallerRef, CodeIntelIndex, CodeIntelStatus, DependencyView, ImportRef, OutlineSummary,
    SymbolRef,
};
use crate::command::{FileSliceKind, GitCommand, ParsedCommand, parse_command};
use crate::output::{ExecResult, OutputOptions, push_budgeted_line};

pub fn execute_mcp(args: McpArgs) -> Result<ExecResult> {
    let root = canonical_root(&args.root)?;
    std::env::set_current_dir(&root)
        .with_context(|| format!("failed to enter MCP root {}", root.display()))?;
    let index = CodeIntelIndex::build(&root)?;
    let session = McpSession {
        root,
        index: RefCell::new(index),
    };
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();

    for line in stdin.lock().lines() {
        let line = line.context("failed to read MCP stdin")?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(&line) {
            Ok(request) => handle_message(&session, &mut stdout, request)?,
            Err(_) => write_error(&mut stdout, Value::Null, -32700, "Parse error")?,
        }
    }

    Ok(ExecResult::success(Vec::new()))
}

struct McpSession {
    root: PathBuf,
    index: RefCell<CodeIntelIndex>,
}

impl McpSession {
    fn with_index<T>(
        &self,
        operation: impl FnOnce(&CodeIntelIndex) -> Result<T>,
    ) -> std::result::Result<T, McpError> {
        let mut index = self.index.borrow_mut();
        index.refresh_if_stale().map_err(McpError::internal)?;
        operation(&index).map_err(McpError::internal)
    }
}

fn handle_message(session: &McpSession, out: &mut impl Write, request: Value) -> Result<()> {
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(Value::as_str);

    match method {
        Some("initialize") => {
            let id = id.unwrap_or(Value::Null);
            write_result(
                out,
                id,
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "agentgrep", "version": env!("CARGO_PKG_VERSION") }
                }),
            )
        }
        Some("notifications/initialized") => Ok(()),
        Some("tools/list") => {
            let id = id.unwrap_or(Value::Null);
            write_result(out, id, json!({ "tools": tools() }))
        }
        Some("tools/call") => {
            let id = id.unwrap_or(Value::Null);
            match handle_tool_call(session, &request) {
                Ok(text) => write_result(out, id, tool_text_result(text)),
                Err(error) => write_error(out, id, error.code, &error.message),
            }
        }
        Some(_) => {
            if let Some(id) = id {
                write_error(out, id, -32601, "Method not found")
            } else {
                Ok(())
            }
        }
        None => {
            if let Some(id) = id {
                write_error(out, id, -32600, "Invalid Request")
            } else {
                Ok(())
            }
        }
    }
}

fn tools() -> Value {
    json!([
        {
            "name": "agentgrep_status",
            "description": "Cached root index status for the persistent MCP session: file count, symbol count, import count, refresh sequence, and latest build time.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "budget": { "type": "integer", "minimum": 1, "default": 1000 }
                },
                "required": [],
                "additionalProperties": false
            }
        },
        {
            "name": "agentgrep_schema",
            "description": "Runtime schema introspection for agentgrep MCP tools. Pass a tool name to inspect one schema, or omit it to list all current schemas.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tool": { "type": "string" },
                    "budget": { "type": "integer", "minimum": 1, "default": 4000 }
                },
                "required": [],
                "additionalProperties": false
            }
        },
        {
            "name": "agentgrep_search",
            "description": "Compact regex or literal search across source files. Returns file paths, line numbers, matching lines, and nearby context.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "paths": { "type": "array", "items": { "type": "string" }, "default": ["."] },
                    "exact": { "type": "boolean", "default": false },
                    "limit": { "type": "integer", "minimum": 1, "default": 8 },
                    "budget": { "type": "integer", "minimum": 1, "default": 4000 }
                },
                "required": ["pattern"],
                "additionalProperties": false
            }
        },
        {
            "name": "agentgrep_context",
            "description": "One-call orientation for a task: compact repo map, ranked search results, and cached symbol/caller hints for the query.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "path": { "type": "string", "default": "." },
                    "exact": { "type": "boolean", "default": false },
                    "limit": { "type": "integer", "minimum": 1, "default": 8 },
                    "budget": { "type": "integer", "minimum": 1, "default": 4000 }
                },
                "required": ["query"],
                "additionalProperties": false
            }
        },
        {
            "name": "agentgrep_file",
            "description": "Read a text file or bounded line range with large-file summarization.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "lines": { "type": "string", "description": "Optional 1-based range like 72:112" },
                    "limit": { "type": "integer", "minimum": 1, "default": 8 },
                    "budget": { "type": "integer", "minimum": 1, "default": 4000 }
                },
                "required": ["path"],
                "additionalProperties": false
            }
        },
        {
            "name": "agentgrep_map",
            "description": "Compact filtered repository map that hides generated, vendor, build, lock, and binary files.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "default": "." },
                    "limit": { "type": "integer", "minimum": 1, "default": 8 },
                    "budget": { "type": "integer", "minimum": 1, "default": 4000 }
                },
                "required": [],
                "additionalProperties": false
            }
        },
        {
            "name": "agentgrep_outline",
            "description": "Cached structural outline for one file: imports and symbols with line numbers.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "default": 20 },
                    "budget": { "type": "integer", "minimum": 1, "default": 3000 }
                },
                "required": ["path"],
                "additionalProperties": false
            }
        },
        {
            "name": "agentgrep_symbol",
            "description": "Find symbol definitions in the cached root index. Matches exact, prefix, substring, and signature text.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "default": 10 },
                    "budget": { "type": "integer", "minimum": 1, "default": 3000 }
                },
                "required": ["query"],
                "additionalProperties": false
            }
        },
        {
            "name": "agentgrep_callers",
            "description": "Find call sites for a symbol using the cached index and source text, excluding definition lines.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "default": 20 },
                    "budget": { "type": "integer", "minimum": 1, "default": 3000 }
                },
                "required": ["symbol"],
                "additionalProperties": false
            }
        },
        {
            "name": "agentgrep_deps",
            "description": "Dependency edges for one indexed file: direct imports, local files that import it, and project manifest paths.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "default": 20 },
                    "budget": { "type": "integer", "minimum": 1, "default": 3000 }
                },
                "required": ["path"],
                "additionalProperties": false
            }
        },
        {
            "name": "agentgrep_run",
            "description": "Run a supported read-only agentgrep command family through the proxy. Mutating git, unsupported commands, and unsafe shell syntax are rejected.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "exact": { "type": "boolean", "default": false },
                    "limit": { "type": "integer", "minimum": 1, "default": 8 },
                    "budget": { "type": "integer", "minimum": 1, "default": 4000 }
                },
                "required": ["command"],
                "additionalProperties": false
            }
        }
    ])
}

fn handle_tool_call(
    session: &McpSession,
    request: &Value,
) -> std::result::Result<String, McpError> {
    let params = object_field(request, "params")?;
    let name = string_field(params, "name")?;
    let args = params
        .get("arguments")
        .and_then(Value::as_object)
        .ok_or_else(|| McpError::invalid("arguments must be an object"))?;

    match name {
        "agentgrep_status" => call_status(session, args),
        "agentgrep_schema" => call_schema(args),
        "agentgrep_search" => call_search(session, args),
        "agentgrep_context" => call_context(session, args),
        "agentgrep_file" => call_file(session, args),
        "agentgrep_map" => call_map(session, args),
        "agentgrep_outline" => call_outline(session, args),
        "agentgrep_symbol" => call_symbol(session, args),
        "agentgrep_callers" => call_callers(session, args),
        "agentgrep_deps" => call_code_deps(session, args),
        "agentgrep_run" => call_run(session, args),
        _ => Err(McpError::method_not_found(format!("Unknown tool: {name}"))),
    }
}

fn call_status(
    session: &McpSession,
    args: &serde_json::Map<String, Value>,
) -> std::result::Result<String, McpError> {
    reject_unknown_fields(args, &["budget"])?;
    let budget = optional_usize_field(args, "budget")?.unwrap_or(1000);
    session.with_index(|index| Ok(render_status(&index.status(), budget)))
}

fn call_schema(args: &serde_json::Map<String, Value>) -> std::result::Result<String, McpError> {
    reject_unknown_fields(args, &["tool", "budget"])?;
    let tool_name = optional_string_field(args, "tool")?;
    let schema = match tool_name {
        Some(tool_name) => tools()
            .as_array()
            .and_then(|tools| tools.iter().find(|tool| tool["name"] == tool_name))
            .cloned()
            .ok_or_else(|| McpError::invalid(format!("unknown tool schema: {tool_name}")))?,
        None => json!({ "tools": tools() }),
    };
    let mut text =
        serde_json::to_string_pretty(&schema).map_err(|error| McpError::internal(error.into()))?;
    let budget = optional_usize_field(args, "budget")?.unwrap_or(4000);
    let max_bytes = budget.saturating_mul(4);
    if text.len() > max_bytes {
        text.truncate(max_bytes);
        text.push_str("\n... truncated; pass a tool name for a smaller schema.\n");
    }
    Ok(text)
}

fn call_search(
    session: &McpSession,
    args: &serde_json::Map<String, Value>,
) -> std::result::Result<String, McpError> {
    reject_unknown_fields(args, &["pattern", "paths", "exact", "limit", "budget"])?;
    let pattern = non_empty_string_field(args, "pattern")?;
    let paths =
        path_array_field(session, args, "paths")?.unwrap_or_else(|| vec![PathBuf::from(".")]);
    let options = output_options(args)?;
    let result = crate::search::execute_regex(
        pattern,
        &paths,
        options,
        Some(format!("agentgrep_search {pattern:?}")),
    )
    .map_err(McpError::internal)?;
    Ok(exec_text(result))
}

fn call_context(
    session: &McpSession,
    args: &serde_json::Map<String, Value>,
) -> std::result::Result<String, McpError> {
    reject_unknown_fields(args, &["query", "path", "exact", "limit", "budget"])?;
    let query = non_empty_string_field(args, "query")?;
    let path = path_field(session, args, "path", ".")?;
    let limit = optional_usize_field(args, "limit")?.unwrap_or(8);
    let budget = optional_usize_field(args, "budget")?.unwrap_or(4000);
    let exact = optional_bool_field(args, "exact")?.unwrap_or(false);

    let map_options = OutputOptions {
        limit,
        budget: budget.saturating_div(4).max(1),
        ..OutputOptions::default()
    };
    let search_options = OutputOptions {
        exact,
        limit,
        budget: budget.saturating_div(2).max(1),
        ..OutputOptions::default()
    };
    let intel_budget = budget.saturating_div(4).max(1);

    let map = crate::repo_map::execute_map(
        &path,
        map_options,
        Some(format!("agentgrep_context map {}", path.display())),
    )
    .map_err(McpError::internal)?;
    let search = crate::search::execute_regex(
        query,
        std::slice::from_ref(&path),
        search_options,
        Some(format!("agentgrep_context search {query:?}")),
    )
    .map_err(McpError::internal)?;
    let intelligence = session.with_index(|index| {
        let symbols = index.symbols(query, limit);
        let callers = match symbols.first() {
            Some(symbol) => index.callers(&symbol.name, limit)?,
            None => Vec::new(),
        };
        Ok(render_context_intel(
            query,
            &symbols,
            &callers,
            intel_budget,
        ))
    })?;

    Ok(format!(
        "## Repo Map\n{}\n## Search Results\n{}\n## Code Intelligence\n{}",
        exec_text(map).trim_end(),
        exec_text(search).trim_end(),
        intelligence.trim_end()
    ))
}

fn call_file(
    session: &McpSession,
    args: &serde_json::Map<String, Value>,
) -> std::result::Result<String, McpError> {
    reject_unknown_fields(args, &["path", "lines", "limit", "budget"])?;
    let path = validated_path(session, string_field(args, "path")?, "path")?;
    let lines = optional_string_field(args, "lines")?;
    let options = output_options(args)?;
    let result =
        crate::file_view::execute_file_with_label(&path, lines, options, Some("agentgrep_file"))
            .map_err(McpError::internal)?;
    Ok(exec_text(result))
}

fn call_map(
    session: &McpSession,
    args: &serde_json::Map<String, Value>,
) -> std::result::Result<String, McpError> {
    reject_unknown_fields(args, &["path", "limit", "budget"])?;
    let path = path_field(session, args, "path", ".")?;
    let options = output_options(args)?;
    let result = crate::repo_map::execute_map(
        &path,
        options,
        Some(format!("agentgrep_map {}", path.display())),
    )
    .map_err(McpError::internal)?;
    Ok(exec_text(result))
}

fn call_outline(
    session: &McpSession,
    args: &serde_json::Map<String, Value>,
) -> std::result::Result<String, McpError> {
    reject_unknown_fields(args, &["path", "limit", "budget"])?;
    let path = validated_path(session, string_field(args, "path")?, "path")?;
    let limit = optional_usize_field(args, "limit")?.unwrap_or(20);
    let budget = optional_usize_field(args, "budget")?.unwrap_or(3000);
    session.with_index(|index| {
        let outline = index.outline(&path, limit)?;
        Ok(render_outline(&outline, budget))
    })
}

fn call_symbol(
    session: &McpSession,
    args: &serde_json::Map<String, Value>,
) -> std::result::Result<String, McpError> {
    reject_unknown_fields(args, &["query", "limit", "budget"])?;
    let query = non_empty_string_field(args, "query")?;
    let limit = optional_usize_field(args, "limit")?.unwrap_or(10);
    let budget = optional_usize_field(args, "budget")?.unwrap_or(3000);
    session.with_index(|index| {
        let symbols = index.symbols(query, limit);
        Ok(render_symbol_results(query, &symbols, budget))
    })
}

fn call_callers(
    session: &McpSession,
    args: &serde_json::Map<String, Value>,
) -> std::result::Result<String, McpError> {
    reject_unknown_fields(args, &["symbol", "limit", "budget"])?;
    let symbol = non_empty_string_field(args, "symbol")?;
    let limit = optional_usize_field(args, "limit")?.unwrap_or(20);
    let budget = optional_usize_field(args, "budget")?.unwrap_or(3000);
    session.with_index(|index| {
        let callers = index.callers(symbol, limit)?;
        Ok(render_callers(symbol, &callers, budget))
    })
}

fn call_code_deps(
    session: &McpSession,
    args: &serde_json::Map<String, Value>,
) -> std::result::Result<String, McpError> {
    reject_unknown_fields(args, &["path", "limit", "budget"])?;
    let path = validated_path(session, string_field(args, "path")?, "path")?;
    let limit = optional_usize_field(args, "limit")?.unwrap_or(20);
    let budget = optional_usize_field(args, "budget")?.unwrap_or(3000);
    session.with_index(|index| {
        let deps = index.deps(&path, limit)?;
        Ok(render_deps(&deps, budget))
    })
}

fn call_run(
    session: &McpSession,
    args: &serde_json::Map<String, Value>,
) -> std::result::Result<String, McpError> {
    reject_unknown_fields(args, &["command", "exact", "limit", "budget"])?;
    let command = string_field(args, "command")?;
    ensure_safe_run_command(session, command)?;
    let options = output_options(args)?;
    let result = crate::tee::with_tee_disabled(|| {
        crate::trace::with_trace_disabled(|| {
            crate::tracking::with_tracking_disabled(|| crate::run::execute_run(command, options))
        })
    })
    .map_err(McpError::internal)?;
    Ok(exec_text(result))
}

fn ensure_safe_run_command(
    session: &McpSession,
    command: &str,
) -> std::result::Result<(), McpError> {
    let parsed = parse_command(command).map_err(|error| McpError::invalid(error.to_string()))?;
    if has_unsafe_shell_syntax(command, &parsed) {
        return Err(McpError::invalid(
            "command contains shell control or redirection syntax",
        ));
    }
    match parsed {
        ParsedCommand::Git(GitCommand::Mutating { .. }) => Err(McpError::invalid(
            "mutating git commands are rejected by MCP",
        )),
        ParsedCommand::Unsupported { reason } => Err(McpError::invalid(reason)),
        parsed => validate_command_paths(session, &parsed),
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

fn validate_command_paths(
    session: &McpSession,
    parsed: &ParsedCommand,
) -> std::result::Result<(), McpError> {
    match parsed {
        ParsedCommand::Search(command) => {
            for path in &command.paths {
                validated_path(session, path, "command path")?;
            }
        }
        ParsedCommand::FindMap { query } => {
            validated_path(session, &query.path, "command path")?;
        }
        ParsedCommand::LsRecursive { path }
        | ParsedCommand::TreeMap { path }
        | ParsedCommand::Cat { path }
        | ParsedCommand::Deps { path } => {
            validated_path(session, path, "command path")?;
        }
        ParsedCommand::FileSlice(slice) => {
            validated_path(session, &slice.path, "command path")?;
        }
        ParsedCommand::WcLines { paths } => {
            for path in paths {
                validated_path(session, path, "command path")?;
            }
        }
        ParsedCommand::Git(_) | ParsedCommand::Test(_) => {}
        ParsedCommand::Unsupported { .. } => {
            unreachable!("unsupported commands are rejected first")
        }
    }
    Ok(())
}

fn path_field(
    session: &McpSession,
    args: &serde_json::Map<String, Value>,
    name: &str,
    default: &str,
) -> std::result::Result<PathBuf, McpError> {
    validated_path(
        session,
        optional_string_field(args, name)?.unwrap_or(default),
        name,
    )
}

fn canonical_root(path: &Path) -> Result<PathBuf> {
    let root = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    root.canonicalize()
        .with_context(|| format!("failed to resolve MCP root {}", path.display()))
}

fn validated_path(
    session: &McpSession,
    path: impl AsRef<Path>,
    label: &str,
) -> std::result::Result<PathBuf, McpError> {
    let path = path.as_ref();
    reject_suspicious_path(path, label)?;
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        session.root.join(path)
    };
    let canonical = candidate.canonicalize().map_err(|_| {
        McpError::invalid(format!(
            "{label} does not exist under MCP root: {}",
            path.display()
        ))
    })?;
    if !canonical.starts_with(&session.root) {
        return Err(McpError::invalid(format!(
            "{label} escapes MCP root: {}",
            path.display()
        )));
    }
    let relative = canonical
        .strip_prefix(&session.root)
        .ok()
        .filter(|relative| !relative.as_os_str().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    reject_sensitive_path(&relative, label)?;
    Ok(relative)
}

fn reject_suspicious_path(path: &Path, label: &str) -> std::result::Result<(), McpError> {
    let text = path.to_string_lossy();
    if text.is_empty() || text.chars().any(char::is_control) {
        return Err(McpError::invalid(format!("{label} is invalid")));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(McpError::invalid(format!(
            "{label} must stay inside the MCP root"
        )));
    }
    Ok(())
}

fn reject_sensitive_path(path: &Path, label: &str) -> std::result::Result<(), McpError> {
    for component in path.components() {
        let Component::Normal(part) = component else {
            continue;
        };
        let name = part.to_string_lossy().to_ascii_lowercase();
        if is_sensitive_path_component(&name) {
            return Err(McpError::invalid(format!(
                "{label} points at sensitive material blocked by MCP: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn is_sensitive_path_component(name: &str) -> bool {
    matches!(name, ".aws" | ".ssh" | ".env" | "credentials.json")
        || matches!(name, "id_rsa" | "id_dsa" | "id_ecdsa" | "id_ed25519")
        || [".env.", ".env-", ".env_", "secret.", "secrets."]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        || matches!(
            Path::new(name)
                .extension()
                .and_then(|extension| extension.to_str()),
            Some("pem" | "key" | "p12" | "pfx")
        )
}

fn output_options(
    args: &serde_json::Map<String, Value>,
) -> std::result::Result<OutputOptions, McpError> {
    Ok(OutputOptions {
        raw: false,
        json: false,
        exact: optional_bool_field(args, "exact")?.unwrap_or(false),
        limit: optional_usize_field(args, "limit")?.unwrap_or(8),
        budget: optional_usize_field(args, "budget")?.unwrap_or(4000),
    }
    .normalized())
}

fn exec_text(result: ExecResult) -> String {
    let mut text = String::from_utf8_lossy(&result.stdout).into_owned();
    if !result.stderr.is_empty() {
        if !text.ends_with('\n') && !text.is_empty() {
            text.push('\n');
        }
        text.push_str("stderr:\n");
        text.push_str(&String::from_utf8_lossy(&result.stderr));
    }
    text
}

fn render_status(status: &CodeIntelStatus, budget: usize) -> String {
    let mut out = String::new();
    let mut truncated = false;
    push_budgeted_line(&mut out, "agentgrep status", budget, &mut truncated);
    push_budgeted_line(
        &mut out,
        &format!("root: {}", status.root),
        budget,
        &mut truncated,
    );
    push_budgeted_line(
        &mut out,
        &format!(
            "indexed: {} file(s), {} byte(s), {} symbol(s), {} import(s)",
            status.files, status.bytes, status.symbols, status.imports
        ),
        budget,
        &mut truncated,
    );
    push_budgeted_line(
        &mut out,
        &format!(
            "sequence: {}, refreshes: {}, last_build_ms: {:.2}, indexed_unix: {}",
            status.sequence, status.refreshes, status.build_ms, status.indexed_unix
        ),
        budget,
        &mut truncated,
    );
    out
}

fn render_outline(outline: &OutlineSummary, budget: usize) -> String {
    let mut out = String::new();
    let mut truncated = false;
    push_budgeted_line(
        &mut out,
        &format!("agentgrep outline: {}", outline.path),
        budget,
        &mut truncated,
    );
    push_budgeted_line(
        &mut out,
        &format!("{} byte(s), {} line(s)", outline.bytes, outline.lines),
        budget,
        &mut truncated,
    );
    render_import_section(
        &mut out,
        "Imports",
        &outline.imports,
        budget,
        &mut truncated,
    );
    render_symbol_section(
        &mut out,
        "Symbols",
        &outline.symbols,
        budget,
        &mut truncated,
    );
    if outline.truncated || truncated {
        out.push_str("Truncated: outline omitted entries. Increase limit or budget.\n");
    }
    out
}

fn render_symbol_results(query: &str, symbols: &[SymbolRef], budget: usize) -> String {
    let mut out = String::new();
    let mut truncated = false;
    push_budgeted_line(
        &mut out,
        &format!("agentgrep symbol: {query}"),
        budget,
        &mut truncated,
    );
    if symbols.is_empty() {
        push_budgeted_line(
            &mut out,
            "No symbol definitions found.",
            budget,
            &mut truncated,
        );
        return out;
    }
    render_symbol_section(&mut out, "Definitions", symbols, budget, &mut truncated);
    if truncated {
        out.push_str("Truncated: symbol results omitted entries. Increase limit or budget.\n");
    }
    out
}

fn render_context_intel(
    query: &str,
    symbols: &[SymbolRef],
    callers: &[CallerRef],
    budget: usize,
) -> String {
    let mut out = String::new();
    let mut truncated = false;
    if symbols.is_empty() && callers.is_empty() {
        push_budgeted_line(
            &mut out,
            &format!("No cached symbol or caller hits for {query:?}."),
            budget,
            &mut truncated,
        );
        return out;
    }
    render_symbol_section(&mut out, "Definitions", symbols, budget, &mut truncated);
    if !callers.is_empty() && push_budgeted_line(&mut out, "Callers:", budget, &mut truncated) {
        for caller in callers {
            if !push_budgeted_line(
                &mut out,
                &format!(
                    "  {}:{} | {}",
                    caller.path,
                    caller.line_number,
                    caller.line.trim()
                ),
                budget,
                &mut truncated,
            ) {
                break;
            }
        }
    }
    if truncated {
        out.push_str(
            "Truncated: code intelligence results omitted entries. Increase limit or budget.\n",
        );
    }
    out
}

fn render_callers(symbol: &str, callers: &[CallerRef], budget: usize) -> String {
    let mut out = String::new();
    let mut truncated = false;
    push_budgeted_line(
        &mut out,
        &format!("agentgrep callers: {symbol}"),
        budget,
        &mut truncated,
    );
    if callers.is_empty() {
        push_budgeted_line(&mut out, "No call sites found.", budget, &mut truncated);
        return out;
    }
    for caller in callers {
        let owner = caller
            .enclosing_symbol
            .as_ref()
            .map(|name| format!(" in {name}"))
            .unwrap_or_default();
        if !push_budgeted_line(
            &mut out,
            &format!(
                "{}:{}{} | {}",
                caller.path,
                caller.line_number,
                owner,
                caller.line.trim()
            ),
            budget,
            &mut truncated,
        ) {
            break;
        }
    }
    if truncated {
        out.push_str("Truncated: caller results omitted entries. Increase limit or budget.\n");
    }
    out
}

fn render_deps(deps: &DependencyView, budget: usize) -> String {
    let mut out = String::new();
    let mut truncated = false;
    push_budgeted_line(
        &mut out,
        &format!("agentgrep deps: {}", deps.path),
        budget,
        &mut truncated,
    );
    render_import_section(&mut out, "Imports", &deps.imports, budget, &mut truncated);
    render_import_section(
        &mut out,
        "Imported by",
        &deps.imported_by,
        budget,
        &mut truncated,
    );
    if !deps.manifests.is_empty() {
        push_budgeted_line(&mut out, "Manifests:", budget, &mut truncated);
        for manifest in &deps.manifests {
            if !push_budgeted_line(&mut out, &format!("  {manifest}"), budget, &mut truncated) {
                break;
            }
        }
    }
    if deps.truncated || truncated {
        out.push_str("Truncated: dependency results omitted entries. Increase limit or budget.\n");
    }
    out
}

fn render_symbol_section(
    out: &mut String,
    label: &str,
    symbols: &[SymbolRef],
    budget: usize,
    truncated: &mut bool,
) {
    if symbols.is_empty() {
        return;
    }
    if !push_budgeted_line(out, &format!("{label}:"), budget, truncated) {
        return;
    }
    for symbol in symbols {
        if !push_budgeted_line(
            out,
            &format!(
                "  {}:{} {} {} | {}",
                symbol.path,
                symbol.line_number,
                symbol.kind,
                symbol.name,
                symbol.signature.trim()
            ),
            budget,
            truncated,
        ) {
            break;
        }
    }
}

fn render_import_section(
    out: &mut String,
    label: &str,
    imports: &[ImportRef],
    budget: usize,
    truncated: &mut bool,
) {
    if imports.is_empty() {
        return;
    }
    if !push_budgeted_line(out, &format!("{label}:"), budget, truncated) {
        return;
    }
    for import in imports {
        if !push_budgeted_line(
            out,
            &format!(
                "  {}:{} -> {} | {}",
                import.path,
                import.line_number,
                import.target,
                import.raw.trim()
            ),
            budget,
            truncated,
        ) {
            break;
        }
    }
}

fn reject_unknown_fields(
    object: &serde_json::Map<String, Value>,
    allowed: &[&str],
) -> std::result::Result<(), McpError> {
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(McpError::invalid(format!("unexpected argument: {key}")));
        }
    }
    Ok(())
}

fn object_field<'a>(
    object: &'a Value,
    name: &str,
) -> std::result::Result<&'a serde_json::Map<String, Value>, McpError> {
    object
        .get(name)
        .and_then(Value::as_object)
        .ok_or_else(|| McpError::invalid(format!("{name} must be an object")))
}

fn string_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    name: &str,
) -> std::result::Result<&'a str, McpError> {
    object
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| McpError::invalid(format!("{name} must be a string")))
}

fn non_empty_string_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    name: &str,
) -> std::result::Result<&'a str, McpError> {
    let value = string_field(object, name)?;
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(McpError::invalid(format!(
            "{name} must be a non-empty string"
        )));
    }
    Ok(value)
}

fn optional_string_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    name: &str,
) -> std::result::Result<Option<&'a str>, McpError> {
    match object.get(name) {
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| McpError::invalid(format!("{name} must be a string"))),
        None => Ok(None),
    }
}

fn optional_bool_field(
    object: &serde_json::Map<String, Value>,
    name: &str,
) -> std::result::Result<Option<bool>, McpError> {
    match object.get(name) {
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| McpError::invalid(format!("{name} must be a boolean"))),
        None => Ok(None),
    }
}

fn optional_usize_field(
    object: &serde_json::Map<String, Value>,
    name: &str,
) -> std::result::Result<Option<usize>, McpError> {
    match object.get(name) {
        Some(value) => {
            let Some(value) = value.as_u64() else {
                return Err(McpError::invalid(format!(
                    "{name} must be a positive integer"
                )));
            };
            usize::try_from(value)
                .ok()
                .filter(|value| *value > 0)
                .map(Some)
                .ok_or_else(|| McpError::invalid(format!("{name} must be a positive integer")))
        }
        None => Ok(None),
    }
}

fn path_array_field(
    session: &McpSession,
    object: &serde_json::Map<String, Value>,
    name: &str,
) -> std::result::Result<Option<Vec<PathBuf>>, McpError> {
    let Some(value) = object.get(name) else {
        return Ok(None);
    };
    let Some(values) = value.as_array() else {
        return Err(McpError::invalid(format!("{name} must be an array")));
    };
    values
        .iter()
        .map(|value| {
            let value = value
                .as_str()
                .ok_or_else(|| McpError::invalid(format!("{name} entries must be strings")))?;
            validated_path(session, value, name)
        })
        .collect::<std::result::Result<Vec<_>, _>>()
        .map(Some)
}

fn tool_text_result(text: String) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false
    })
}

fn write_result(out: &mut impl Write, id: Value, result: Value) -> Result<()> {
    writeln!(
        out,
        "{}",
        json!({ "jsonrpc": "2.0", "id": id, "result": result })
    )?;
    out.flush()?;
    Ok(())
}

fn write_error(out: &mut impl Write, id: Value, code: i32, message: &str) -> Result<()> {
    writeln!(
        out,
        "{}",
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message }
        })
    )?;
    out.flush()?;
    Ok(())
}

#[derive(Debug)]
struct McpError {
    code: i32,
    message: String,
}

impl McpError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
        }
    }

    fn method_not_found(message: impl Into<String>) -> Self {
        Self {
            code: -32601,
            message: message.into(),
        }
    }

    fn internal(error: anyhow::Error) -> Self {
        Self {
            code: -32000,
            message: format!("{error:#}"),
        }
    }
}
