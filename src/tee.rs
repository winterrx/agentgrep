use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const MIN_TEE_BYTES: usize = 500;
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
    let mut bytes = Vec::with_capacity(raw_len + 64);
    bytes.extend_from_slice(stdout);
    if !stderr.is_empty() {
        if !stdout.ends_with(b"\n") && !stdout.is_empty() {
            bytes.push(b'\n');
        }
        bytes.extend_from_slice(b"\n--- stderr ---\n");
        bytes.extend_from_slice(stderr);
    }
    std::fs::write(&path, bytes).ok()?;
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
    std::env::var_os("AGENTGREP_TEE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".agentgrep/tee"))
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
}
