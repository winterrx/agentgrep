use std::{
    collections::BTreeMap,
    env, fs,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

pub const TRACKING_ENV: &str = "AGENTGREP_TRACKING";
pub const TRACKING_PATH_ENV: &str = "AGENTGREP_TRACKING_PATH";
pub const DEFAULT_TRACKING_PATH: &str = ".agentgrep/tracking.sqlite";
pub const LEGACY_TRACKING_PATH: &str = ".agentgrep/tracking.jsonl";
static TRACKING_DISABLED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackingConfig {
    pub enabled: bool,
    pub path: PathBuf,
}

impl TrackingConfig {
    pub fn from_env() -> Self {
        let enabled = env_flag(TRACKING_ENV).unwrap_or(true);
        let path = env::var_os(TRACKING_PATH_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_TRACKING_PATH));
        Self { enabled, path }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrackingRecord {
    pub command: String,
    pub optimized_command_label: String,
    pub cwd: String,
    pub project: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub saved_tokens: i64,
    pub savings_pct: f64,
    pub elapsed_ms: u64,
    pub timestamp: i64,
}

#[derive(Debug, Clone)]
pub struct TrackingInput {
    pub command: String,
    pub optimized_command_label: String,
    pub cwd: PathBuf,
    pub project: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub baseline_output_tokens: Option<u64>,
    pub elapsed_ms: u64,
}

impl TrackingRecord {
    pub fn from_input(input: TrackingInput) -> Self {
        let baseline = input.baseline_output_tokens.unwrap_or(input.input_tokens);
        let saved_tokens = baseline as i64 - input.output_tokens as i64;
        let savings_pct = if baseline == 0 {
            0.0
        } else {
            (saved_tokens as f64 / baseline as f64) * 100.0
        };
        let project = input.project.unwrap_or_else(|| {
            project_name(&input.cwd).unwrap_or_else(|| input.cwd.display().to_string())
        });
        Self {
            command: sanitize_command(&input.command),
            optimized_command_label: input.optimized_command_label,
            cwd: input.cwd.display().to_string(),
            project,
            input_tokens: input.input_tokens,
            output_tokens: input.output_tokens,
            saved_tokens,
            savings_pct,
            elapsed_ms: input.elapsed_ms,
            timestamp: now_unix_ms(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrackingSummary {
    pub total_records: usize,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_saved_tokens: i64,
    pub avg_savings_pct: f64,
    pub by_command: Vec<TrackingGroupSummary>,
    pub by_project: Vec<TrackingGroupSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrackingGroupSummary {
    pub key: String,
    pub records: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub saved_tokens: i64,
    pub avg_savings_pct: f64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Default)]
struct GroupAccumulator {
    records: usize,
    input_tokens: u64,
    output_tokens: u64,
    saved_tokens: i64,
    savings_pct_total: f64,
    elapsed_ms: u64,
}

impl GroupAccumulator {
    fn add(&mut self, record: &TrackingRecord) {
        self.records += 1;
        self.input_tokens += record.input_tokens;
        self.output_tokens += record.output_tokens;
        self.saved_tokens += record.saved_tokens;
        self.savings_pct_total += record.savings_pct;
        self.elapsed_ms += record.elapsed_ms;
    }

    fn into_summary(self, key: String) -> TrackingGroupSummary {
        TrackingGroupSummary {
            key,
            records: self.records,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            saved_tokens: self.saved_tokens,
            avg_savings_pct: average(self.savings_pct_total, self.records),
            elapsed_ms: self.elapsed_ms,
        }
    }
}

pub fn append_tracking_record(record: &TrackingRecord) -> Result<()> {
    let config = TrackingConfig::from_env();
    append_tracking_record_with_config(&config, record)
}

pub fn append_tracking_record_with_config(
    config: &TrackingConfig,
    record: &TrackingRecord,
) -> Result<()> {
    if !config.enabled || TRACKING_DISABLED.load(Ordering::SeqCst) {
        return Ok(());
    }
    append_record_to_path(&config.path, record)
}

pub fn with_tracking_disabled<T>(f: impl FnOnce() -> T) -> T {
    let _guard = TrackingDisableGuard {
        previous: TRACKING_DISABLED.swap(true, Ordering::SeqCst),
    };
    f()
}

struct TrackingDisableGuard {
    previous: bool,
}

impl Drop for TrackingDisableGuard {
    fn drop(&mut self) {
        TRACKING_DISABLED.store(self.previous, Ordering::SeqCst);
    }
}

pub fn append_record_to_path(path: &Path, record: &TrackingRecord) -> Result<()> {
    if is_jsonl_path(path) {
        return append_jsonl_record_to_path(path, record);
    }
    append_sqlite_record_to_path(path, record)
}

fn append_sqlite_record_to_path(path: &Path, record: &TrackingRecord) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create tracking dir {}", parent.display()))?;
    }
    let _guard = LedgerLock::acquire(path)?;
    let connection = open_tracking_db(path)?;
    connection.execute(
        "INSERT INTO tracking_records (
            command,
            optimized_command_label,
            cwd,
            project,
            input_tokens,
            output_tokens,
            saved_tokens,
            savings_pct,
            elapsed_ms,
            timestamp
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            &record.command,
            &record.optimized_command_label,
            &record.cwd,
            &record.project,
            record.input_tokens as i64,
            record.output_tokens as i64,
            record.saved_tokens,
            record.savings_pct,
            record.elapsed_ms as i64,
            record.timestamp,
        ],
    )?;
    Ok(())
}

fn append_jsonl_record_to_path(path: &Path, record: &TrackingRecord) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create tracking dir {}", parent.display()))?;
    }
    let _guard = LedgerLock::acquire(path)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open tracking ledger {}", path.display()))?;
    serde_json::to_writer(&mut file, record)?;
    file.write_all(b"\n")?;
    Ok(())
}

pub fn load_tracking_records(path: &Path) -> Result<Vec<TrackingRecord>> {
    if !path.exists() && !is_jsonl_path(path) && Path::new(LEGACY_TRACKING_PATH).exists() {
        return load_jsonl_tracking_records(Path::new(LEGACY_TRACKING_PATH));
    }
    if is_jsonl_path(path) {
        return load_jsonl_tracking_records(path);
    }
    load_sqlite_tracking_records(path)
}

fn load_jsonl_tracking_records(path: &Path) -> Result<Vec<TrackingRecord>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read tracking ledger {}", path.display()))?;
    let records = content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    Ok(records)
}

fn load_sqlite_tracking_records(path: &Path) -> Result<Vec<TrackingRecord>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let connection = open_tracking_db(path)?;
    let mut statement = connection.prepare(
        "SELECT command,
                optimized_command_label,
                cwd,
                project,
                input_tokens,
                output_tokens,
                saved_tokens,
                savings_pct,
                elapsed_ms,
                timestamp
         FROM tracking_records
         ORDER BY id ASC",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(TrackingRecord {
            command: row.get(0)?,
            optimized_command_label: row.get(1)?,
            cwd: row.get(2)?,
            project: row.get(3)?,
            input_tokens: row.get::<_, i64>(4)?.max(0) as u64,
            output_tokens: row.get::<_, i64>(5)?.max(0) as u64,
            saved_tokens: row.get(6)?,
            savings_pct: row.get(7)?,
            elapsed_ms: row.get::<_, i64>(8)?.max(0) as u64,
            timestamp: row.get(9)?,
        })
    })?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row?);
    }
    Ok(records)
}

fn open_tracking_db(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path)
        .with_context(|| format!("failed to open tracking sqlite db {}", path.display()))?;
    initialize_tracking_db(&connection)?;
    Ok(connection)
}

fn initialize_tracking_db(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        PRAGMA busy_timeout = 5000;
        CREATE TABLE IF NOT EXISTS tracking_records (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            command TEXT NOT NULL,
            optimized_command_label TEXT NOT NULL,
            cwd TEXT NOT NULL,
            project TEXT NOT NULL,
            input_tokens INTEGER NOT NULL,
            output_tokens INTEGER NOT NULL,
            saved_tokens INTEGER NOT NULL,
            savings_pct REAL NOT NULL,
            elapsed_ms INTEGER NOT NULL,
            timestamp INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_tracking_records_command
            ON tracking_records(command);
        CREATE INDEX IF NOT EXISTS idx_tracking_records_project
            ON tracking_records(project);
        CREATE INDEX IF NOT EXISTS idx_tracking_records_timestamp
            ON tracking_records(timestamp);
        ",
    )?;
    Ok(())
}

fn is_jsonl_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("jsonl" | "json")
    )
}

struct LedgerLock {
    path: PathBuf,
}

impl LedgerLock {
    fn acquire(ledger_path: &Path) -> Result<Self> {
        let lock_path = ledger_path.with_extension("jsonl.lock");
        let started = SystemTime::now();
        loop {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(mut lock) => {
                    let pid = std::process::id();
                    let _ = writeln!(lock, "{pid}");
                    return Ok(Self { path: lock_path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    remove_stale_lock(&lock_path);
                    let waited = started.elapsed().unwrap_or_default();
                    if waited > Duration::from_secs(5) {
                        anyhow::bail!(
                            "timed out waiting for tracking ledger lock {}",
                            lock_path.display()
                        );
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "failed to create tracking ledger lock {}",
                            lock_path.display()
                        )
                    });
                }
            }
        }
    }
}

impl Drop for LedgerLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn remove_stale_lock(lock_path: &Path) {
    let Ok(metadata) = fs::metadata(lock_path) else {
        return;
    };
    let Ok(modified) = metadata.modified() else {
        return;
    };
    if modified
        .elapsed()
        .is_ok_and(|age| age > Duration::from_secs(30))
    {
        let _ = fs::remove_file(lock_path);
    }
}

pub fn summarize_tracking_records(records: &[TrackingRecord]) -> TrackingSummary {
    let mut total = GroupAccumulator::default();
    let mut by_command = BTreeMap::new();
    let mut by_project = BTreeMap::new();
    for record in records {
        total.add(record);
        by_command
            .entry(record.command.clone())
            .or_insert_with(GroupAccumulator::default)
            .add(record);
        by_project
            .entry(record.project.clone())
            .or_insert_with(GroupAccumulator::default)
            .add(record);
    }

    TrackingSummary {
        total_records: total.records,
        total_input_tokens: total.input_tokens,
        total_output_tokens: total.output_tokens,
        total_saved_tokens: total.saved_tokens,
        avg_savings_pct: average(total.savings_pct_total, total.records),
        by_command: sorted_groups(by_command),
        by_project: sorted_groups(by_project),
    }
}

pub fn summarize_tracking_path(path: &Path) -> Result<TrackingSummary> {
    let records = load_tracking_records(path)?;
    Ok(summarize_tracking_records(&records))
}

pub fn sanitize_command(command: &str) -> String {
    let Ok(words) = shell_words::split(command) else {
        return redact_inline_secrets(command);
    };
    let mut sanitized = Vec::with_capacity(words.len());
    let mut redact_next = false;
    for word in words {
        if redact_next {
            sanitized.push("[REDACTED]".to_string());
            redact_next = false;
            continue;
        }
        if is_secret_key_value(&word) {
            sanitized.push(redact_key_value(&word));
        } else if is_secret_flag(&word) {
            sanitized.push(word);
            redact_next = true;
        } else {
            sanitized.push(word);
        }
    }
    sanitized.join(" ")
}

fn sorted_groups(groups: BTreeMap<String, GroupAccumulator>) -> Vec<TrackingGroupSummary> {
    let mut summaries: Vec<_> = groups
        .into_iter()
        .map(|(key, group)| group.into_summary(key))
        .collect();
    summaries.sort_by(|left, right| {
        right
            .saved_tokens
            .cmp(&left.saved_tokens)
            .then_with(|| right.records.cmp(&left.records))
            .then_with(|| left.key.cmp(&right.key))
    });
    summaries
}

fn average(total: f64, count: usize) -> f64 {
    if count == 0 {
        0.0
    } else {
        total / count as f64
    }
}

fn project_name(cwd: &Path) -> Option<String> {
    cwd.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
}

fn env_flag(name: &str) -> Option<bool> {
    match env::var(name).ok()?.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

fn redact_inline_secrets(command: &str) -> String {
    command
        .split_whitespace()
        .map(|word| {
            if is_secret_key_value(word) {
                redact_key_value(word)
            } else {
                word.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_secret_key_value(word: &str) -> bool {
    let Some((key, value)) = word.split_once('=') else {
        return false;
    };
    !value.is_empty() && secretish(key)
}

fn redact_key_value(word: &str) -> String {
    let Some((key, _value)) = word.split_once('=') else {
        return "[REDACTED]".to_string();
    };
    format!("{key}=[REDACTED]")
}

fn is_secret_flag(word: &str) -> bool {
    if !word.starts_with('-') || word.contains('=') {
        return false;
    }
    let key = word.trim_start_matches('-').trim_end_matches([':', '=']);
    secretish(key)
}

fn secretish(key: &str) -> bool {
    let normalized = key.trim_start_matches('-').to_ascii_lowercase();
    [
        "secret",
        "token",
        "password",
        "passwd",
        "apikey",
        "api-key",
        "api_key",
        "private-key",
        "private_key",
        "access-key",
        "access_key",
    ]
    .iter()
    .any(|needle| {
        normalized == *needle
            || normalized.ends_with(&format!("-{needle}"))
            || normalized.ends_with(&format!("_{needle}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn appends_and_loads_jsonl_records() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".agentgrep/tracking.jsonl");
        let record = TrackingRecord::from_input(TrackingInput {
            command: "rg token --password hunter2".to_string(),
            optimized_command_label: "agentgrep search".to_string(),
            cwd: tmp.path().join("repo"),
            project: Some("repo".to_string()),
            input_tokens: 100,
            output_tokens: 40,
            baseline_output_tokens: Some(120),
            elapsed_ms: 7,
        });

        append_record_to_path(&path, &record).unwrap();
        append_record_to_path(&path, &record).unwrap();

        let loaded = load_tracking_records(&path).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].saved_tokens, 80);
        assert_eq!(loaded[0].savings_pct, 80.0 / 120.0 * 100.0);
        assert_eq!(loaded[0].command, "rg token --password [REDACTED]");
    }

    #[test]
    fn appends_and_loads_sqlite_records() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".agentgrep/tracking.sqlite");
        let first = record("rg foo", "repo", 100, 20, 7);
        let second = record("git status", "repo", 40, 10, 3);

        append_record_to_path(&path, &first).unwrap();
        append_record_to_path(&path, &second).unwrap();

        let loaded = load_tracking_records(&path).unwrap();
        assert_eq!(loaded, vec![first.clone(), second.clone()]);
        let summary = summarize_tracking_path(&path).unwrap();
        assert_eq!(summary.total_records, 2);
        assert_eq!(summary.total_saved_tokens, 110);
        assert_eq!(summary.by_command[0].key, "rg foo");
    }

    #[test]
    fn load_skips_malformed_jsonl_records() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("tracking.jsonl");
        let valid = record("rg foo", "repo", 10, 2, 1);
        let valid_json = serde_json::to_string(&valid).unwrap();
        fs::write(&path, format!("{valid_json}\n{{not json\n{valid_json}\n")).unwrap();

        let loaded = load_tracking_records(&path).unwrap();

        assert_eq!(loaded, vec![valid.clone(), valid]);
    }

    #[test]
    fn concurrent_appends_do_not_interleave_records() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("tracking.sqlite");
        let mut handles = Vec::new();
        for idx in 0..8 {
            let path = path.clone();
            handles.push(std::thread::spawn(move || {
                for item in 0..10 {
                    append_record_to_path(
                        &path,
                        &record(&format!("rg foo-{idx}-{item}"), "repo", 10, 2, 1),
                    )
                    .unwrap();
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }

        let loaded = load_tracking_records(&path).unwrap();

        assert_eq!(loaded.len(), 80);
    }

    #[test]
    fn summarizes_by_command_and_project() {
        let records = vec![
            record("rg foo", "alpha", 100, 25, 10),
            record("rg foo", "alpha", 80, 30, 20),
            record("git status", "beta", 40, 10, 5),
        ];

        let summary = summarize_tracking_records(&records);

        assert_eq!(summary.total_records, 3);
        assert_eq!(summary.total_input_tokens, 220);
        assert_eq!(summary.total_output_tokens, 65);
        assert_eq!(summary.total_saved_tokens, 155);
        assert_eq!(summary.by_command[0].key, "rg foo");
        assert_eq!(summary.by_command[0].records, 2);
        assert_eq!(summary.by_project[0].key, "alpha");
        assert_eq!(summary.by_project[0].saved_tokens, 125);
    }

    #[test]
    fn env_config_can_disable_and_override_path() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("custom.jsonl");
        unsafe {
            env::set_var(TRACKING_ENV, "false");
            env::set_var(TRACKING_PATH_ENV, &path);
        }

        let config = TrackingConfig::from_env();
        assert!(!config.enabled);
        assert_eq!(config.path, path);
        append_tracking_record_with_config(&config, &record("rg foo", "repo", 10, 1, 1)).unwrap();
        assert!(!config.path.exists());

        unsafe {
            env::remove_var(TRACKING_ENV);
            env::remove_var(TRACKING_PATH_ENV);
        }
    }

    #[test]
    fn default_config_uses_sqlite_tracking_db() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            env::remove_var(TRACKING_ENV);
            env::remove_var(TRACKING_PATH_ENV);
        }

        let config = TrackingConfig::from_env();

        assert_eq!(config.path, PathBuf::from(DEFAULT_TRACKING_PATH));
    }

    #[test]
    fn sanitizes_key_value_and_flag_secrets() {
        assert_eq!(
            sanitize_command("TOKEN=abc rg foo --api-key sk-123 --literal ok password=bad"),
            "TOKEN=[REDACTED] rg foo --api-key [REDACTED] --literal ok password=[REDACTED]"
        );
    }

    fn record(
        command: &str,
        project: &str,
        input_tokens: u64,
        output_tokens: u64,
        elapsed_ms: u64,
    ) -> TrackingRecord {
        TrackingRecord::from_input(TrackingInput {
            command: command.to_string(),
            optimized_command_label: "optimized".to_string(),
            cwd: PathBuf::from(format!("/tmp/{project}")),
            project: Some(project.to_string()),
            input_tokens,
            output_tokens,
            baseline_output_tokens: Some(input_tokens),
            elapsed_ms,
        })
    }
}
