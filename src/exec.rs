use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use std::{env, io, thread};

use anyhow::{Context, Result};

const DEFAULT_OPTIMIZED_STDOUT_CAPTURE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct CapturedCommand {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: i32,
    pub duration: Duration,
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

impl CapturedCommand {
    pub fn output_tokens(&self) -> usize {
        crate::output::estimate_tokens_from_bytes(self.stdout_bytes + self.stderr_bytes)
    }

    pub fn capture_hint(&self, recovery_hint: Option<&str>) -> Option<String> {
        let mut hint = String::new();
        if self.stdout_truncated {
            hint.push_str(&format!(
                "Raw capture: stdout truncated after {} of {} byte(s). Use --raw for exact output.\n",
                self.stdout.len(),
                self.stdout_bytes
            ));
        }
        if self.stderr_truncated {
            hint.push_str(&format!(
                "Raw capture: stderr truncated after {} of {} byte(s). Use --raw for exact output.\n",
                self.stderr.len(),
                self.stderr_bytes
            ));
        }
        if let Some(recovery_hint) = recovery_hint {
            hint.push_str(recovery_hint);
        } else if hint.ends_with('\n') {
            hint.pop();
        }
        (!hint.is_empty()).then_some(hint)
    }
}

pub fn run_shell_capture(command: &str, cwd: Option<&Path>) -> Result<CapturedCommand> {
    run_shell_capture_with_path(command, cwd, None)
}

pub fn run_shell_capture_real_tools(command: &str, cwd: Option<&Path>) -> Result<CapturedCommand> {
    let path = path_without_agentgrep_shim_dirs();
    run_shell_capture_with_path(command, cwd, path.as_deref())
}

pub fn run_shell_capture_optimized_real_tools(
    command: &str,
    cwd: Option<&Path>,
) -> Result<CapturedCommand> {
    let path = path_without_agentgrep_shim_dirs();
    run_shell_capture_with_path_and_limits(
        command,
        cwd,
        path.as_deref(),
        optimized_stdout_capture_limit(),
        None,
    )
}

fn run_shell_capture_with_path(
    command: &str,
    cwd: Option<&Path>,
    path: Option<&std::ffi::OsStr>,
) -> Result<CapturedCommand> {
    run_shell_capture_with_path_and_limits(command, cwd, path, None, None)
}

fn run_shell_capture_with_path_and_limits(
    command: &str,
    cwd: Option<&Path>,
    path: Option<&std::ffi::OsStr>,
    stdout_limit: Option<usize>,
    stderr_limit: Option<usize>,
) -> Result<CapturedCommand> {
    let start = Instant::now();
    let mut cmd = shell_command(command);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    if let Some(path) = path {
        cmd.env("PATH", path);
    }
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to execute shell command: {command}"))?;
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let stdout_handle = thread::spawn(move || read_stream(stdout, stdout_limit));
    let stderr_handle = thread::spawn(move || read_stream(stderr, stderr_limit));
    let status = child
        .wait()
        .with_context(|| format!("failed to execute shell command: {command}"))?;
    let stdout = stdout_handle
        .join()
        .map_err(|_| anyhow::anyhow!("stdout reader panicked"))??;
    let stderr = stderr_handle
        .join()
        .map_err(|_| anyhow::anyhow!("stderr reader panicked"))??;

    Ok(CapturedCommand {
        stdout: stdout.bytes,
        stderr: stderr.bytes,
        exit_code: status.code().unwrap_or(1),
        duration: start.elapsed(),
        stdout_bytes: stdout.total_bytes,
        stderr_bytes: stderr.total_bytes,
        stdout_truncated: stdout.truncated,
        stderr_truncated: stderr.truncated,
    })
}

#[derive(Debug)]
struct StreamCapture {
    bytes: Vec<u8>,
    total_bytes: usize,
    truncated: bool,
}

fn read_stream<R: io::Read>(mut reader: R, limit: Option<usize>) -> io::Result<StreamCapture> {
    let mut bytes = Vec::new();
    let mut total_bytes = 0usize;
    let mut truncated = false;
    let mut chunk = [0u8; 8192];
    loop {
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        total_bytes += read;
        match limit {
            Some(limit) if bytes.len() < limit => {
                let remaining = limit - bytes.len();
                let keep = remaining.min(read);
                bytes.extend_from_slice(&chunk[..keep]);
                truncated |= keep < read;
            }
            Some(_) => {
                truncated = true;
            }
            None => bytes.extend_from_slice(&chunk[..read]),
        }
    }
    Ok(StreamCapture {
        bytes,
        total_bytes,
        truncated,
    })
}

fn optimized_stdout_capture_limit() -> Option<usize> {
    match env::var("AGENTGREP_CAPTURE_MAX_STDOUT_BYTES") {
        Ok(value) if value == "0" || value.eq_ignore_ascii_case("off") => None,
        Ok(value) => value
            .parse()
            .ok()
            .or(Some(DEFAULT_OPTIMIZED_STDOUT_CAPTURE_BYTES)),
        Err(_) => Some(DEFAULT_OPTIMIZED_STDOUT_CAPTURE_BYTES),
    }
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
