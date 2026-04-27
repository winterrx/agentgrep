use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::cli::{ShimExecArgs, ShimsArgs, ShimsCommands, ShimsDirArgs, ShimsInstallArgs};
use crate::output::{ExecResult, OutputOptions};

const SHIM_MARKER: &str = "# agentgrep shim v1";
const SHIM_COMMANDS: &[&str] = &[
    "rg", "grep", "find", "ls", "cat", "git", "head", "tail", "sed", "nl", "wc", "tree",
];

pub fn execute_shims(args: ShimsArgs) -> Result<ExecResult> {
    match args.command {
        ShimsCommands::Install(args) => install(args),
        ShimsCommands::Uninstall(args) => uninstall(args),
        ShimsCommands::Status(args) => status(args),
    }
}

pub fn execute_shim_exec(args: ShimExecArgs) -> Result<ExecResult> {
    if !SHIM_COMMANDS.contains(&args.program.as_str()) {
        bail!("unsupported shim command: {}", args.program);
    }
    let program = resolve_real_program(&args.program)?;
    let command = shell_command(&program.display().to_string(), &args.args);
    let display_command = shell_command(&args.program, &args.args);
    crate::run::execute_run_with_trace_label(
        &command,
        &display_command,
        OutputOptions::default(),
        None,
    )
}

fn install(args: ShimsInstallArgs) -> Result<ExecResult> {
    let dir = expand_tilde(&args.dir)?;
    let agentgrep = match args.agentgrep {
        Some(path) => expand_tilde(&path)?,
        None => env::current_exe().context("failed to locate current agentgrep binary")?,
    };
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;

    let mut installed = Vec::new();
    let mut skipped = Vec::new();
    for command in SHIM_COMMANDS {
        let path = dir.join(command);
        if path.exists() && !is_agentgrep_shim(&path)? && !args.force {
            skipped.push(format!(
                "{} (exists; pass --force to overwrite)",
                path.display()
            ));
            continue;
        }
        fs::write(&path, shim_script(command, &agentgrep)?)
            .with_context(|| format!("failed to write {}", path.display()))?;
        make_executable(&path)?;
        installed.push(path.display().to_string());
    }

    let mut out = String::new();
    out.push_str("agentgrep shims install\n");
    out.push_str(&format!("dir: {}\n", dir.display()));
    out.push_str(&format!("agentgrep: {}\n", agentgrep.display()));
    out.push_str(&format!("installed: {}\n", installed.len()));
    for path in installed {
        out.push_str(&format!("  {path}\n"));
    }
    if !skipped.is_empty() {
        out.push_str(&format!("skipped: {}\n", skipped.len()));
        for path in skipped {
            out.push_str(&format!("  {path}\n"));
        }
        out.push_str("Exit code: 1\n");
        return Ok(ExecResult::from_parts(out, Vec::new(), 1));
    }
    if !path_has_dir(&dir) {
        out.push_str("PATH: not active; prepend this directory to PATH to use the shims.\n");
    } else {
        out.push_str("PATH: active\n");
    }
    out.push_str("Exit code: 0\n");
    Ok(ExecResult::success(out))
}

fn uninstall(args: ShimsDirArgs) -> Result<ExecResult> {
    let dir = expand_tilde(&args.dir)?;
    let mut removed = Vec::new();
    let mut skipped = Vec::new();
    for command in SHIM_COMMANDS {
        let path = dir.join(command);
        if !path.exists() {
            continue;
        }
        if !is_agentgrep_shim(&path)? {
            skipped.push(path.display().to_string());
            continue;
        }
        fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))?;
        removed.push(path.display().to_string());
    }

    let mut out = String::new();
    out.push_str("agentgrep shims uninstall\n");
    out.push_str(&format!("dir: {}\n", dir.display()));
    out.push_str(&format!("removed: {}\n", removed.len()));
    for path in removed {
        out.push_str(&format!("  {path}\n"));
    }
    if !skipped.is_empty() {
        out.push_str(&format!("skipped non-agentgrep files: {}\n", skipped.len()));
        for path in skipped {
            out.push_str(&format!("  {path}\n"));
        }
        out.push_str("Exit code: 1\n");
        return Ok(ExecResult::from_parts(out, Vec::new(), 1));
    }
    out.push_str("Exit code: 0\n");
    Ok(ExecResult::success(out))
}

fn status(args: ShimsDirArgs) -> Result<ExecResult> {
    let dir = expand_tilde(&args.dir)?;
    let mut installed = 0;
    let mut conflicts = 0;
    let mut out = String::new();
    out.push_str("agentgrep shims status\n");
    out.push_str(&format!("dir: {}\n", dir.display()));
    out.push_str(&format!(
        "PATH: {}\n",
        if path_has_dir(&dir) {
            "active"
        } else {
            "not active"
        }
    ));
    for command in SHIM_COMMANDS {
        let path = dir.join(command);
        let state = if !path.exists() {
            "missing"
        } else if is_agentgrep_shim(&path)? {
            installed += 1;
            "installed"
        } else {
            conflicts += 1;
            "conflict"
        };
        out.push_str(&format!("{command}: {state}\n"));
    }
    out.push_str(&format!("installed: {installed}/{}\n", SHIM_COMMANDS.len()));
    out.push_str(&format!("conflicts: {conflicts}\n"));
    out.push_str(&format!(
        "Exit code: {}\n",
        if conflicts == 0 { 0 } else { 1 }
    ));
    Ok(ExecResult::from_parts(
        out,
        Vec::new(),
        if conflicts == 0 { 0 } else { 1 },
    ))
}

fn shell_command(program: &str, args: &[String]) -> String {
    let mut command = shell_words::quote(program).to_string();
    for arg in args {
        command.push(' ');
        command.push_str(&shell_words::quote(arg));
    }
    command
}

fn shim_script(command: &str, agentgrep: &Path) -> Result<String> {
    let agentgrep = agentgrep
        .to_str()
        .ok_or_else(|| anyhow!("agentgrep path is not valid UTF-8: {}", agentgrep.display()))?;
    Ok(format!(
        r#"#!/bin/sh
{marker}
shim_dir=$(dirname "$0")
case "$shim_dir" in
  /*) ;;
  *) shim_dir=$(pwd)/$shim_dir ;;
esac
shim_dir=$(cd "$shim_dir" 2>/dev/null && pwd -P)
new_path=
old_ifs=$IFS
IFS=:
for entry in $PATH; do
  if [ "$entry" = "$shim_dir" ]; then
    continue
  fi
  if [ -z "$new_path" ]; then
    new_path=$entry
  else
    new_path=$new_path:$entry
  fi
done
IFS=$old_ifs
export PATH=$new_path
export AGENTGREP_SHIM_DIR=$shim_dir
export AGENTGREP_SHIM_ACTIVE=1
exec {agentgrep} shim-exec {command} -- "$@"
"#,
        marker = SHIM_MARKER,
        agentgrep = shell_single_quote(agentgrep),
        command = shell_single_quote(command),
    ))
}

fn resolve_real_program(program: &str) -> Result<PathBuf> {
    let shim_dir = env::var_os("AGENTGREP_SHIM_DIR").map(PathBuf::from);
    for entry in env::split_paths(&env::var_os("PATH").unwrap_or_default()) {
        if shim_dir
            .as_deref()
            .map(|shim_dir| same_path(&entry, shim_dir))
            .unwrap_or(false)
        {
            continue;
        }
        let candidate = entry.join(program);
        if !is_executable_file(&candidate) {
            continue;
        }
        if is_agentgrep_shim(&candidate).unwrap_or(false) {
            continue;
        }
        return Ok(candidate);
    }
    bail!("could not find real executable for shimmed command: {program}")
}

fn is_agentgrep_shim(path: &Path) -> Result<bool> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(content.contains(SHIM_MARKER))
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

fn path_has_dir(dir: &Path) -> bool {
    let Ok(target) = canonical_or_self(dir) else {
        return false;
    };
    env::split_paths(&env::var_os("PATH").unwrap_or_default()).any(|entry| {
        canonical_or_self(&entry)
            .map(|entry| entry == target)
            .unwrap_or(false)
    })
}

fn canonical_or_self(path: &Path) -> Result<PathBuf> {
    Ok(path.canonicalize().unwrap_or_else(|_| path.to_path_buf()))
}

fn same_path(left: &Path, right: &Path) -> bool {
    match (canonical_or_self(left), canonical_or_self(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)
        .with_context(|| format!("failed to stat {}", path.display()))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("failed to chmod {}", path.display()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    bail!("agentgrep shims currently require a POSIX shell")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_command_quotes_arguments() {
        assert_eq!(
            shell_command(
                "rg",
                &["hello world".to_string(), "src/main.rs".to_string()]
            ),
            "rg 'hello world' src/main.rs"
        );
    }

    #[test]
    fn shim_script_removes_its_dir_from_path() {
        let script = shim_script("rg", Path::new("/tmp/agentgrep")).unwrap();
        assert!(script.contains(SHIM_MARKER));
        assert!(script.contains("export PATH=$new_path"));
        assert!(script.contains("export AGENTGREP_SHIM_DIR=$shim_dir"));
        assert!(script.contains("shim-exec 'rg' -- \"$@\""));
    }
}
