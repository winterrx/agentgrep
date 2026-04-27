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
    let display_command = shell_command(&args.program, &args.args);
    let command = match resolve_real_program(&args.program)? {
        Some(program) => shell_command(&program.display().to_string(), &args.args),
        None if args.program == "tree" => display_command.clone(),
        None => bail!(
            "could not find real executable for shimmed command: {}",
            args.program
        ),
    };
    crate::run::execute_run_with_trace_label(
        &command,
        &display_command,
        OutputOptions::from_env_defaults(),
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
        let real_program = resolve_real_program(command)?;
        fs::write(
            &path,
            shim_script(command, &agentgrep, real_program.as_deref())?,
        )
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
    out.push_str(&format!("PATH: {}\n", path_activation_summary(&dir)));
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
    let path_present = path_has_dir(&dir);
    out.push_str("agentgrep shims status\n");
    out.push_str(&format!("dir: {}\n", dir.display()));
    out.push_str(&format!("PATH: {}\n", path_activation_summary(&dir)));
    for command in SHIM_COMMANDS {
        let path = dir.join(command);
        let state = if !path.exists() {
            "missing".to_string()
        } else if is_agentgrep_shim(&path)? {
            installed += 1;
            if !path_present {
                "installed (not on PATH)".to_string()
            } else if let Some(shadow) = shadowing_executable(command, &path) {
                format!("installed (shadowed by {})", shadow.display())
            } else {
                "installed".to_string()
            }
        } else {
            conflicts += 1;
            "conflict".to_string()
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

fn shim_script(command: &str, agentgrep: &Path, real_program: Option<&Path>) -> Result<String> {
    let agentgrep = agentgrep
        .to_str()
        .ok_or_else(|| anyhow!("agentgrep path is not valid UTF-8: {}", agentgrep.display()))?;
    let stdin_passthrough = match real_program {
        Some(real_program) => {
            let real_program = real_program.to_str().ok_or_else(|| {
                anyhow!(
                    "real executable path is not valid UTF-8: {}",
                    real_program.display()
                )
            })?;
            format!(
                "if [ -p /dev/stdin ] || {{ [ ! -t 0 ] && [ -s /dev/stdin ]; }}; then\n  exec {} \"$@\"\nfi\n",
                shell_single_quote(real_program)
            )
        }
        None => String::new(),
    };
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
{stdin_passthrough}
exec {agentgrep} shim-exec {command} -- "$@"
"#,
        marker = SHIM_MARKER,
        stdin_passthrough = stdin_passthrough,
        agentgrep = shell_single_quote(agentgrep),
        command = shell_single_quote(command),
    ))
}

fn resolve_real_program(program: &str) -> Result<Option<PathBuf>> {
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
        return Ok(Some(candidate));
    }
    Ok(None)
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

fn path_activation_summary(dir: &Path) -> String {
    if !path_has_dir(dir) {
        return format!(
            "not active; prepend {} to PATH to use the shims.",
            dir.display()
        );
    }

    let shadowed = shadowed_commands(dir);
    if shadowed.is_empty() {
        "active".to_string()
    } else {
        let (command, path) = &shadowed[0];
        format!(
            "present but shadowed; prepend {} before system paths (for example, {command} resolves to {}).",
            dir.display(),
            path.display()
        )
    }
}

fn shadowed_commands(dir: &Path) -> Vec<(&'static str, PathBuf)> {
    SHIM_COMMANDS
        .iter()
        .filter_map(|command| {
            let shim = dir.join(command);
            if !shim.exists() || !is_agentgrep_shim(&shim).unwrap_or(false) {
                return None;
            }
            shadowing_executable(command, &shim).map(|path| (*command, path))
        })
        .collect()
}

fn shadowing_executable(command: &str, shim: &Path) -> Option<PathBuf> {
    let first = first_executable_on_path(command)?;
    (!same_path(&first, shim)).then_some(first)
}

fn first_executable_on_path(command: &str) -> Option<PathBuf> {
    for entry in env::split_paths(&env::var_os("PATH").unwrap_or_default()) {
        let candidate = entry.join(command);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
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
        let script = shim_script(
            "rg",
            Path::new("/tmp/agentgrep"),
            Some(Path::new("/opt/bin/rg")),
        )
        .unwrap();
        assert!(script.contains(SHIM_MARKER));
        assert!(script.contains("export PATH=$new_path"));
        assert!(script.contains("export AGENTGREP_SHIM_DIR=$shim_dir"));
        assert!(script.contains("exec '/opt/bin/rg' \"$@\""));
        assert!(script.contains("shim-exec 'rg' -- \"$@\""));
    }
}
