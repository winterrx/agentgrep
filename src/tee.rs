use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const MIN_TEE_BYTES: usize = 500;
const DEFAULT_MAX_TEE_FILES: usize = 20;
const DEFAULT_MAX_TEE_BYTES: usize = 1_048_576;
static TEE_DISABLED: AtomicBool = AtomicBool::new(false);

pub fn tee_raw_output(command: &str, stdout: &[u8], stderr: &[u8], force: bool) -> Option<String> {
    if TEE_DISABLED.load(Ordering::SeqCst) {
        return None;
    }
    if std::env::var("AGENTGREP_TEE").ok().as_deref() == Some("0") {
        return None;
    }
    let raw_len = stdout.len() + stderr.len();
    if !force && raw_len < MIN_TEE_BYTES {
        return None;
    }
    if force && raw_len == 0 {
        return None;
    }

    let dir = tee_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let epoch = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let path = dir.join(format!("{}_{}.log", epoch, sanitize(command)));
    let max_bytes = max_tee_bytes();
    let mut bytes = Vec::with_capacity(raw_len + 64);
    bytes.extend_from_slice(stdout);
    if !stderr.is_empty() {
        if !stdout.ends_with(b"\n") && !stdout.is_empty() {
            bytes.push(b'\n');
        }
        bytes.extend_from_slice(b"\n--- stderr ---\n");
        bytes.extend_from_slice(stderr);
    }
    truncate_bytes_at_char_boundary(&mut bytes, max_bytes);
    std::fs::write(&path, bytes).ok()?;
    cleanup_old_files(&dir, max_tee_files());
    Some(format!("Full output: {}", display_path(&path)))
}

pub fn with_tee_disabled<T>(f: impl FnOnce() -> T) -> T {
    let _guard = TeeDisableGuard {
        previous: TEE_DISABLED.swap(true, Ordering::SeqCst),
    };
    f()
}

struct TeeDisableGuard {
    previous: bool,
}

impl Drop for TeeDisableGuard {
    fn drop(&mut self) {
        TEE_DISABLED.store(self.previous, Ordering::SeqCst);
    }
}

fn tee_dir() -> PathBuf {
    let base = std::env::var_os("AGENTGREP_TEE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(default_tee_base_dir);
    base.join(project_slug())
}

fn max_tee_files() -> usize {
    std::env::var("AGENTGREP_TEE_MAX_FILES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_MAX_TEE_FILES)
        .max(1)
}

fn max_tee_bytes() -> usize {
    std::env::var("AGENTGREP_TEE_MAX_BYTES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_MAX_TEE_BYTES)
        .max(256)
}

fn truncate_bytes_at_char_boundary(bytes: &mut Vec<u8>, max_bytes: usize) {
    if bytes.len() <= max_bytes {
        return;
    }
    let mut boundary = max_bytes;
    while boundary > 0 && std::str::from_utf8(&bytes[..boundary]).is_err() {
        boundary -= 1;
    }
    bytes.truncate(boundary);
    bytes.extend_from_slice(format!("\n\n--- truncated at {max_bytes} bytes ---\n").as_bytes());
}

fn cleanup_old_files(dir: &Path, max_files: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut logs = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "log"))
        .collect::<Vec<_>>();
    if logs.len() <= max_files {
        return;
    }
    logs.sort_by_key(|entry| entry.file_name());
    let remove_count = logs.len().saturating_sub(max_files);
    for entry in logs.into_iter().take(remove_count) {
        let _ = std::fs::remove_file(entry.path());
    }
}

fn sanitize(command: &str) -> String {
    let mut value: String = command
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    value.truncate(48);
    if value.is_empty() {
        "command".to_string()
    } else {
        value
    }
}

fn default_tee_base_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".agentgrep/tee"))
        .unwrap_or_else(|| PathBuf::from(".agentgrep/tee"))
}

fn project_slug() -> String {
    project_root()
        .or_else(|| std::env::current_dir().ok())
        .and_then(|cwd| {
            cwd.file_name()
                .map(|name| name.to_string_lossy().to_string())
        })
        .map(|name| sanitize(&name))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "unknown-project".to_string())
}

fn project_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn display_path(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_command_slug() {
        assert_eq!(sanitize("rg 'stripe test'"), "rg__stripe_test_");
    }

    #[test]
    fn can_disable_tee_for_benchmarks() {
        let hint = with_tee_disabled(|| tee_raw_output("rg x", b"stdout", b"", true));
        assert!(hint.is_none());
    }

    #[test]
    fn default_tee_dir_is_user_level_and_project_scoped() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let repo = tmp.path().join("repo-name");
        let subdir = repo.join("src");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::write(repo.join(".git"), "gitdir: .git/worktrees/test\n").unwrap();
        let old_home = std::env::var_os("HOME");
        let old_tee_dir = std::env::var_os("AGENTGREP_TEE_DIR");
        let old_cwd = std::env::current_dir().unwrap();
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::remove_var("AGENTGREP_TEE_DIR");
        }
        std::env::set_current_dir(&subdir).unwrap();

        let hint = tee_raw_output("rg x", b"full output", b"", true).unwrap();

        assert!(hint.contains(".agentgrep/tee/repo-name/"));
        assert!(home.join(".agentgrep/tee/repo-name").is_dir());
        assert!(!repo.join(".agentgrep").exists());
        std::env::set_current_dir(old_cwd).unwrap();
        unsafe {
            restore_env("HOME", old_home);
            restore_env("AGENTGREP_TEE_DIR", old_tee_dir);
        }
    }

    #[test]
    fn truncates_tee_bytes() {
        let mut bytes = "abcdef".as_bytes().to_vec();
        truncate_bytes_at_char_boundary(&mut bytes, 3);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("abc"));
        assert!(text.contains("truncated at 3 bytes"));
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    unsafe fn restore_env(name: &str, value: Option<std::ffi::OsString>) {
        if let Some(value) = value {
            unsafe {
                std::env::set_var(name, value);
            }
        } else {
            unsafe {
                std::env::remove_var(name);
            }
        }
    }
}
