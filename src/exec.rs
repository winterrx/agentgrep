use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct CapturedCommand {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: i32,
    pub duration: Duration,
}

pub fn run_shell_capture(command: &str, cwd: Option<&Path>) -> Result<CapturedCommand> {
    run_shell_capture_with_path(command, cwd, None)
}

pub fn run_shell_capture_real_tools(command: &str, cwd: Option<&Path>) -> Result<CapturedCommand> {
    let path = path_without_agentgrep_shim_dirs();
    run_shell_capture_with_path(command, cwd, path.as_deref())
}

fn run_shell_capture_with_path(
    command: &str,
    cwd: Option<&Path>,
    path: Option<&std::ffi::OsStr>,
) -> Result<CapturedCommand> {
    let start = Instant::now();
    let mut cmd = shell_command(command);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    if let Some(path) = path {
        cmd.env("PATH", path);
    }
    let output = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("failed to execute shell command: {command}"))?;

    Ok(CapturedCommand {
        stdout: output.stdout,
        stderr: output.stderr,
        exit_code: output.status.code().unwrap_or(1),
        duration: start.elapsed(),
    })
}

fn path_without_agentgrep_shim_dirs() -> Option<std::ffi::OsString> {
    let paths = std::env::var_os("PATH")?;
    let filtered = std::env::split_paths(&paths)
        .filter(|dir| !contains_agentgrep_shim(dir))
        .collect::<Vec<_>>();
    std::env::join_paths(filtered).ok()
}

fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg(command);
        cmd
    }

    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(command);
        cmd
    }
}

pub fn command_exists(name: &str) -> Option<String> {
    if name.contains(std::path::MAIN_SEPARATOR) {
        let path = Path::new(name);
        return path.is_file().then(|| path.display().to_string());
    }

    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(name);
        if candidate.is_file() && !is_agentgrep_shim(&candidate) {
            return Some(candidate.display().to_string());
        }

        #[cfg(windows)]
        {
            let candidate = dir.join(format!("{name}.exe"));
            if candidate.is_file() && !is_agentgrep_shim(&candidate) {
                return Some(candidate.display().to_string());
            }
        }
    }
    None
}

fn is_agentgrep_shim(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .map(|content| content.contains("# agentgrep shim v1"))
        .unwrap_or(false)
}

fn contains_agentgrep_shim(dir: &Path) -> bool {
    const COMMANDS: &[&str] = &[
        "rg",
        "grep",
        "find",
        "ls",
        "cat",
        "git",
        "head",
        "tail",
        "sed",
        "nl",
        "wc",
        "tree",
        "cargo",
        "pytest",
        "py.test",
        "python",
        "python3",
        "go",
        "npm",
        "pnpm",
        "yarn",
        "npx",
        "vitest",
        "jest",
        "playwright",
        "ruff",
        "mypy",
        "deps",
    ];
    COMMANDS
        .iter()
        .any(|command| is_agentgrep_shim(&dir.join(command)))
}
