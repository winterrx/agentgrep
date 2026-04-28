use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::cli::{
    ClaudeHookScope, ClaudeHooksInstallArgs, CodexHookScope, CodexHooksInstallArgs, HooksArgs,
    HooksCommands,
};
use crate::command::{GitCommand, ParsedCommand, parse_command};
use crate::output::ExecResult;

const CLAUDE_HOOK_STATUS: &str = "Routing safe Bash through agentgrep";
const CODEX_HOOK_STATUS: &str = "Loading agentgrep shell proxy";

pub fn execute_hooks(args: HooksArgs) -> Result<ExecResult> {
    match args.command {
        HooksCommands::InstallClaude(args) => install_claude(args),
        HooksCommands::InstallCodex(args) => install_codex(args),
        HooksCommands::ClaudePreToolUse => handle_claude_pre_tool_use(),
        HooksCommands::CodexPreToolUse => handle_codex_pre_tool_use(),
        HooksCommands::CodexSessionStart => handle_codex_session_start(),
    }
}

fn install_claude(args: ClaudeHooksInstallArgs) -> Result<ExecResult> {
    let agentgrep = resolve_agentgrep(args.agentgrep)?;
    let settings_path = claude_settings_path(args.scope)?;
    let command = hook_command(&agentgrep, "claude-pre-tool-use")?;
    upsert_json_hook(
        &settings_path,
        "PreToolUse",
        Some("Bash"),
        &command,
        5,
        CLAUDE_HOOK_STATUS,
    )?;

    let mut out = String::new();
    out.push_str("agentgrep hooks install-claude\n");
    out.push_str(&format!("scope: {:?}\n", args.scope).to_lowercase());
    out.push_str(&format!("settings: {}\n", settings_path.display()));
    out.push_str(&format!("handler: {command}\n"));
    out.push_str("behavior: rewrites safe Bash tool calls to agentgrep run\n");
    out.push_str("Exit code: 0\n");
    Ok(ExecResult::success(out))
}

fn install_codex(args: CodexHooksInstallArgs) -> Result<ExecResult> {
    let agentgrep = resolve_agentgrep(args.agentgrep)?;
    let dir = codex_config_dir(args.scope)?;
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let hooks_path = dir.join("hooks.json");
    let config_path = dir.join("config.toml");
    let pre_tool_command = hook_command(&agentgrep, "codex-pre-tool-use")?;
    let session_command = hook_command(&agentgrep, "codex-session-start")?;

    upsert_json_hook(
        &hooks_path,
        "PreToolUse",
        Some("^Bash$"),
        &pre_tool_command,
        5,
        "Checking agentgrep proxy",
    )?;
    upsert_json_hook(
        &hooks_path,
        "SessionStart",
        Some("startup|resume|clear"),
        &session_command,
        5,
        CODEX_HOOK_STATUS,
    )?;
    enable_codex_hooks_feature(&config_path)?;

    let mut out = String::new();
    out.push_str("agentgrep hooks install-codex\n");
    out.push_str(&format!("scope: {:?}\n", args.scope).to_lowercase());
    out.push_str(&format!("hooks: {}\n", hooks_path.display()));
    out.push_str(&format!("config: {}\n", config_path.display()));
    out.push_str("behavior: adds agentgrep context; Codex currently does not support PreToolUse updatedInput rewrites\n");
    out.push_str("Exit code: 0\n");
    Ok(ExecResult::success(out))
}

fn handle_claude_pre_tool_use() -> Result<ExecResult> {
    let input = read_stdin_json()?;
    let Some(command) = hook_command_input(&input) else {
        return Ok(ExecResult::success(String::new()));
    };
    let Some(rewritten) = rewrite_command_for_agentgrep(command) else {
        return Ok(ExecResult::success(String::new()));
    };

    let mut updated_input = input
        .get("tool_input")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if let Some(object) = updated_input.as_object_mut() {
        object.insert("command".to_string(), Value::String(rewritten));
    } else {
        updated_input = json!({ "command": rewritten });
    }

    let output = json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "permissionDecisionReason": "agentgrep is proxying this safe command to reduce token output.",
            "updatedInput": updated_input
        }
    });
    Ok(ExecResult::success(format!("{output}\n")))
}

fn handle_codex_pre_tool_use() -> Result<ExecResult> {
    let _input = read_stdin_json()?;
    Ok(ExecResult::success(String::new()))
}

fn handle_codex_session_start() -> Result<ExecResult> {
    let _input = read_stdin_json()?;
    let output = json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": "agentgrep is available in this workspace. Keep using normal shell discovery commands; when agentgrep shims are on PATH they proxy common commands automatically, and complex Bash can be run explicitly as `agentgrep run \"...\"`. Codex PreToolUse hooks cannot rewrite Bash input yet, so shims remain the transparent proxy path."
        }
    });
    Ok(ExecResult::success(format!("{output}\n")))
}

fn read_stdin_json() -> Result<Value> {
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .context("failed to read hook input from stdin")?;
    if input.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&input).context("failed to parse hook input JSON")
}

fn hook_command_input(input: &Value) -> Option<&str> {
    if input.get("tool_name").and_then(Value::as_str) != Some("Bash") {
        return None;
    }
    input
        .get("tool_input")
        .and_then(|value| value.get("command"))
        .and_then(Value::as_str)
}

fn rewrite_command_for_agentgrep(command: &str) -> Option<String> {
    if command.trim().is_empty()
        || command.contains("agentgrep run")
        || executable_name(command).as_deref() == Some("agentgrep")
        || has_shell_control_syntax(command)
    {
        return None;
    }
    let parsed = parse_command(command).ok()?;
    if !is_safe_proxy_candidate(&parsed) {
        return None;
    }
    Some(format!("agentgrep run {}", shell_words::quote(command)))
}

fn is_safe_proxy_candidate(parsed: &ParsedCommand) -> bool {
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
        | ParsedCommand::Git(GitCommand::ReadOnly { .. }) => true,
        ParsedCommand::Git(GitCommand::Mutating { .. }) | ParsedCommand::Unsupported { .. } => {
            false
        }
    }
}

fn has_shell_control_syntax(command: &str) -> bool {
    command.contains("$(")
        || command.contains('`')
        || command.contains("&&")
        || command.contains("||")
        || command.contains(';')
        || command.contains('|')
        || command.contains('>')
        || command.contains('<')
        || command.contains('\n')
}

fn executable_name(command: &str) -> Option<String> {
    let words = shell_words::split(command).ok()?;
    let first = words.first()?;
    Path::new(first)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
}

fn upsert_json_hook(
    path: &Path,
    event: &str,
    matcher: Option<&str>,
    command: &str,
    timeout: u64,
    status_message: &str,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut root = if path.exists() {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if content.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&content)
                .with_context(|| format!("failed to parse {}", path.display()))?
        }
    } else {
        json!({})
    };
    if !root.is_object() {
        bail!("{} must contain a JSON object", path.display());
    }

    {
        let hooks = ensure_object_field(&mut root, "hooks")?;
        let groups = ensure_array_field(hooks, event)?;
        if !json_hook_command_exists(groups, command) {
            let mut group = json!({
                "hooks": [{
                    "type": "command",
                    "command": command,
                    "timeout": timeout,
                    "statusMessage": status_message
                }]
            });
            if let Some(matcher) = matcher {
                group["matcher"] = Value::String(matcher.to_string());
            }
            groups.push(group);
        }
    }

    fs::write(path, format!("{}\n", serde_json::to_string_pretty(&root)?))
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn json_hook_command_exists(groups: &[Value], command: &str) -> bool {
    groups.iter().any(|group| {
        group
            .get("hooks")
            .and_then(Value::as_array)
            .map(|hooks| {
                hooks
                    .iter()
                    .any(|hook| hook.get("command").and_then(Value::as_str) == Some(command))
            })
            .unwrap_or(false)
    })
}

fn ensure_object_field<'a>(
    value: &'a mut Value,
    field: &str,
) -> Result<&'a mut serde_json::Map<String, Value>> {
    let object = value
        .as_object_mut()
        .ok_or_else(|| anyhow!("expected JSON object"))?;
    object.entry(field.to_string()).or_insert_with(|| json!({}));
    object
        .get_mut(field)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| anyhow!("{field} must be a JSON object"))
}

fn ensure_array_field<'a>(
    object: &'a mut serde_json::Map<String, Value>,
    field: &str,
) -> Result<&'a mut Vec<Value>> {
    object.entry(field.to_string()).or_insert_with(|| json!([]));
    object
        .get_mut(field)
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow!("{field} must be a JSON array"))
}

fn enable_codex_hooks_feature(path: &Path) -> Result<()> {
    let content = if path.exists() {
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?
    } else {
        String::new()
    };
    let mut lines: Vec<String> = content.lines().map(ToString::to_string).collect();
    if let Some(index) = lines
        .iter()
        .position(|line| line.trim_start().starts_with("codex_hooks"))
    {
        lines[index] = "codex_hooks = true".to_string();
        fs::write(path, format!("{}\n", lines.join("\n")))
            .with_context(|| format!("failed to write {}", path.display()))?;
        return Ok(());
    }

    if let Some(index) = lines.iter().position(|line| line.trim() == "[features]") {
        lines.insert(index + 1, "codex_hooks = true".to_string());
    } else {
        if !lines.is_empty() && !lines.last().map(|line| line.is_empty()).unwrap_or(false) {
            lines.push(String::new());
        }
        lines.push("[features]".to_string());
        lines.push("codex_hooks = true".to_string());
    }
    fs::write(path, format!("{}\n", lines.join("\n")))
        .with_context(|| format!("failed to write {}", path.display()))
}

fn resolve_agentgrep(path: Option<PathBuf>) -> Result<PathBuf> {
    let path = match path {
        Some(path) => expand_tilde(&path)?,
        None => env::current_exe().context("failed to locate current agentgrep binary")?,
    };
    if path.is_absolute() {
        return Ok(path);
    }
    Ok(env::current_dir()?.join(path))
}

fn hook_command(agentgrep: &Path, subcommand: &str) -> Result<String> {
    let path = agentgrep
        .to_str()
        .ok_or_else(|| anyhow!("agentgrep path is not valid UTF-8: {}", agentgrep.display()))?;
    Ok(format!(
        "{} hooks {}",
        shell_words::quote(path),
        shell_words::quote(subcommand)
    ))
}

fn claude_settings_path(scope: ClaudeHookScope) -> Result<PathBuf> {
    Ok(match scope {
        ClaudeHookScope::User => home_dir()?.join(".claude/settings.json"),
        ClaudeHookScope::Project => project_root()?.join(".claude/settings.json"),
        ClaudeHookScope::Local => project_root()?.join(".claude/settings.local.json"),
    })
}

fn codex_config_dir(scope: CodexHookScope) -> Result<PathBuf> {
    Ok(match scope {
        CodexHookScope::User => home_dir()?.join(".codex"),
        CodexHookScope::Project => project_root()?.join(".codex"),
    })
}

fn project_root() -> Result<PathBuf> {
    let mut dir = env::current_dir().context("failed to read current directory")?;
    loop {
        if dir.join(".git").exists() {
            return Ok(dir);
        }
        if !dir.pop() {
            return env::current_dir().context("failed to read current directory");
        }
    }
}

fn expand_tilde(path: &Path) -> Result<PathBuf> {
    let value = path.display().to_string();
    if value == "~" {
        return home_dir();
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return Ok(home_dir()?.join(rest));
    }
    Ok(path.to_path_buf())
}

fn home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME is not set"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_safe_read_only_command() {
        assert_eq!(
            rewrite_command_for_agentgrep("rg stripe"),
            Some("agentgrep run 'rg stripe'".to_string())
        );
        assert_eq!(
            rewrite_command_for_agentgrep("git status --short"),
            Some("agentgrep run 'git status --short'".to_string())
        );
    }

    #[test]
    fn skips_mutating_unsupported_and_shell_control_commands() {
        assert_eq!(rewrite_command_for_agentgrep("git commit -m ship"), None);
        assert_eq!(rewrite_command_for_agentgrep("rm -rf target"), None);
        assert_eq!(rewrite_command_for_agentgrep("rg stripe | head"), None);
        assert_eq!(
            rewrite_command_for_agentgrep("agentgrep run 'rg stripe'"),
            None
        );
    }

    #[test]
    fn codex_feature_insert_preserves_existing_features_table() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        fs::write(&path, "[features]\nfoo = true\n").unwrap();
        enable_codex_hooks_feature(&path).unwrap();
        let content = fs::read_to_string(path).unwrap();
        assert!(content.contains("[features]\ncodex_hooks = true\nfoo = true"));
    }

    #[test]
    fn codex_feature_replaces_false_value() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        fs::write(&path, "[features]\ncodex_hooks = false\n").unwrap();
        enable_codex_hooks_feature(&path).unwrap();
        let content = fs::read_to_string(path).unwrap();
        assert_eq!(content.matches("codex_hooks").count(), 1);
        assert!(content.contains("codex_hooks = true"));
    }

    #[test]
    fn upsert_json_hook_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        upsert_json_hook(
            &path,
            "PreToolUse",
            Some("Bash"),
            "agentgrep hooks claude-pre-tool-use",
            5,
            "status",
        )
        .unwrap();
        upsert_json_hook(
            &path,
            "PreToolUse",
            Some("Bash"),
            "agentgrep hooks claude-pre-tool-use",
            5,
            "status",
        )
        .unwrap();
        let value: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        let groups = value["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
    }
}
