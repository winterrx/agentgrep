use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::{env, iter};

fn agentgrep() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agentgrep"))
}

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/discovery")
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn has_command(name: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {name} >/dev/null 2>&1"))
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn run_agentgrep(cwd: &Path, args: &[&str]) -> Output {
    run_agentgrep_with_env(cwd, args, &[])
}

fn run_agentgrep_with_env(cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut command = Command::new(agentgrep());
    command.args(args).current_dir(cwd);
    if !envs
        .iter()
        .any(|(key, _)| *key == "AGENTGREP_TRACKING" || *key == "AGENTGREP_TRACKING_PATH")
    {
        command.env("AGENTGREP_TRACKING", "0");
    }
    for (key, value) in envs {
        command.env(key, value);
    }
    command.output().expect("agentgrep command runs")
}

fn run_agentgrep_with_stdin(cwd: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(agentgrep())
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("agentgrep command spawns");
    child
        .stdin
        .as_mut()
        .expect("stdin is piped")
        .write_all(stdin.as_bytes())
        .expect("hook stdin writes");
    child.wait_with_output().expect("agentgrep command runs")
}

#[test]
fn disable_env_bypasses_proxy_optimization() {
    if !has_command("rg") {
        return;
    }
    let cwd = fixture();
    let raw = Command::new("sh")
        .arg("-c")
        .arg("rg --sort path stripe")
        .current_dir(&cwd)
        .output()
        .expect("raw rg runs");
    let proxied = run_agentgrep_with_env(
        &cwd,
        &[
            "run",
            "rg --sort path stripe",
            "--limit",
            "1",
            "--budget",
            "1",
        ],
        &[("AGENTGREP_DISABLE", "1")],
    );

    assert_eq!(proxied.status.code(), raw.status.code());
    assert_eq!(proxied.stdout, raw.stdout);
    assert_eq!(proxied.stderr, raw.stderr);
}

#[test]
fn run_raw_matches_original_command_byte_for_byte() {
    if !has_command("rg") {
        return;
    }
    let cwd = fixture();
    let raw = Command::new("sh")
        .arg("-c")
        .arg("rg --sort path stripe")
        .current_dir(&cwd)
        .output()
        .expect("raw rg runs");
    let proxied = run_agentgrep(&cwd, &["run", "rg --sort path stripe", "--raw"]);

    assert_eq!(proxied.status.code(), raw.status.code());
    assert_eq!(proxied.stdout, raw.stdout);
    assert_eq!(proxied.stderr, raw.stderr);
}

#[test]
fn run_rg_returns_compact_matches_with_context_and_truncation() {
    if !has_command("rg") {
        return;
    }
    let cwd = fixture();
    let tmp = tempfile::tempdir().unwrap();
    let tee_dir = tmp.path().join("tee");
    let tee_dir = tee_dir.to_string_lossy().to_string();
    let output = run_agentgrep_with_env(
        &cwd,
        &[
            "run",
            "rg --sort path stripe",
            "--limit",
            "2",
            "--budget",
            "100",
        ],
        &[("AGENTGREP_TEE_DIR", &tee_dir)],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(stdout.contains("agentgrep optimized: rg --sort path stripe"));
    assert!(stdout.contains("agent-session.jsonl:"));
    assert!(stdout.contains("> |"));
    assert!(stdout.contains("Truncated:"));
    assert!(stdout.contains("Raw: agentgrep run \"rg --sort path stripe\" --raw"));
    assert!(Path::new(&tee_dir).exists());
    assert!(stdout.contains("Exit code: 0"));
}

#[test]
fn optimized_search_reports_when_raw_capture_is_capped() {
    if !has_command("rg") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let mut body = String::new();
    for idx in 0..2000 {
        body.push_str(&format!("stripe capture cap line {idx}\n"));
    }
    fs::write(tmp.path().join("large.txt"), body).unwrap();

    let output = run_agentgrep_with_env(
        tmp.path(),
        &[
            "run",
            "rg --sort path stripe",
            "--limit",
            "3",
            "--budget",
            "80",
        ],
        &[("AGENTGREP_CAPTURE_MAX_STDOUT_BYTES", "256")],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "{stdout}");
    assert!(
        stdout.contains("agentgrep optimized: rg --sort path stripe"),
        "{stdout}"
    );
    assert!(stdout.contains("large.txt:"), "{stdout}");
    assert!(stdout.contains("Truncated:"), "{stdout}");
    assert!(stdout.contains("Raw capture: stdout truncated"), "{stdout}");
    assert!(stdout.contains("Exit code: 0"), "{stdout}");
}

#[test]
fn plain_rg_large_output_uses_internal_fast_path_without_tee() {
    if !has_command("rg") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let tee_dir = tmp.path().join("tee");
    let mut body = String::new();
    for idx in 0..1200 {
        body.push_str(&format!("stripe fast path line {idx}\n"));
    }
    fs::write(tmp.path().join("large.txt"), body).unwrap();

    let output = run_agentgrep_with_env(
        tmp.path(),
        &["run", "rg stripe", "--limit", "3", "--budget", "100"],
        &[("AGENTGREP_TEE_DIR", tee_dir.to_str().unwrap())],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "{stdout}");
    assert!(
        stdout.contains("agentgrep optimized: rg stripe"),
        "{stdout}"
    );
    assert!(stdout.contains("large.txt:"), "{stdout}");
    assert!(stdout.contains("Truncated:"), "{stdout}");
    assert!(stdout.contains("Exit code: 0"), "{stdout}");
    assert!(
        !tee_dir.exists(),
        "internal fast path should not run raw command just to create a tee"
    );
}

#[test]
fn plain_rg_small_output_stays_raw_exact() {
    if !has_command("rg") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("small.txt"), "stripe small output\n").unwrap();

    let raw = Command::new("sh")
        .arg("-c")
        .arg("rg stripe")
        .current_dir(tmp.path())
        .output()
        .expect("raw rg runs");
    let proxied = run_agentgrep(
        tmp.path(),
        &["run", "rg stripe", "--limit", "10", "--budget", "4000"],
    );

    assert_eq!(proxied.status.code(), raw.status.code());
    assert_eq!(proxied.stdout, raw.stdout);
    assert_eq!(proxied.stderr, raw.stderr);
}

#[test]
fn raw_mode_ignores_optimized_capture_cap_and_stays_exact() {
    if !has_command("rg") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let mut body = String::new();
    for idx in 0..500 {
        body.push_str(&format!("stripe raw exact line {idx}\n"));
    }
    fs::write(tmp.path().join("large.txt"), body).unwrap();

    let raw = Command::new("sh")
        .arg("-c")
        .arg("rg stripe")
        .current_dir(tmp.path())
        .output()
        .expect("raw rg runs");
    let proxied = run_agentgrep_with_env(
        tmp.path(),
        &["run", "rg stripe", "--raw"],
        &[("AGENTGREP_CAPTURE_MAX_STDOUT_BYTES", "64")],
    );

    assert_eq!(proxied.status.code(), raw.status.code());
    assert_eq!(proxied.stdout, raw.stdout);
    assert_eq!(proxied.stderr, raw.stderr);
}

#[test]
fn complex_rg_flags_compact_the_raw_result_set() {
    if !has_command("rg") {
        return;
    }
    let cwd = fixture();
    let output = run_agentgrep(
        &cwd,
        &[
            "run",
            "rg -g '*.md' --sort path stripe .",
            "--limit",
            "20",
            "--budget",
            "80",
        ],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(stdout.contains("agentgrep optimized: rg -g '*.md' --sort path stripe ."));
    assert!(stdout.contains("docs/stripe-notes.md:"));
    assert!(!stdout.contains("src/billing/stripe.ts:"));
    assert!(stdout.contains("Exit code: 0"));
}

#[test]
fn invalid_regex_preserves_nonzero_exit_and_stderr() {
    if !has_command("rg") {
        return;
    }
    let cwd = fixture();
    let output = run_agentgrep(&cwd, &["run", "rg '['"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(!stderr.is_empty());
}

#[test]
fn map_hides_generated_vendor_and_build_files() {
    let cwd = fixture();
    let output = run_agentgrep(&cwd, &["map", "."]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(stdout.contains("src/billing/stripe.ts"));
    assert!(stdout.contains("tests/billing.test.ts"));
    assert!(!stdout.contains("vendor/stripe-sdk.js"));
    assert!(!stdout.contains("generated/schema.generated.ts"));
}

#[test]
fn repo_listing_commands_use_filtered_map_even_when_raw_is_small() {
    let cwd = fixture();
    for command in ["find . -type f", "ls -R", "tree -L 2 ."] {
        let output = run_agentgrep(&cwd, &["run", command, "--limit", "50", "--budget", "4000"]);
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(output.status.success(), "{command}: {stdout}");
        assert!(
            stdout.contains("agentgrep optimized:"),
            "{command}: {stdout}"
        );
        assert!(
            stdout.contains("src/billing/stripe.ts"),
            "{command}: {stdout}"
        );
        assert!(
            !stdout.contains("vendor/stripe-sdk.js"),
            "{command}: {stdout}"
        );
        assert!(
            !stdout.contains("generated/schema.generated.ts"),
            "{command}: {stdout}"
        );
        assert!(!stdout.contains(".agentgrep/tee"), "{command}: {stdout}");
        assert!(stdout.contains("Exit code: 0"), "{command}: {stdout}");
    }
}

#[test]
fn find_name_filters_are_honored_by_compact_map() {
    let cwd = fixture();
    let output = run_agentgrep(
        &cwd,
        &[
            "run",
            "find . -type f -name '*.ts'",
            "--limit",
            "50",
            "--budget",
            "4000",
        ],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("agentgrep optimized:"), "{stdout}");
    assert!(stdout.contains("Find filters: name=*.ts"), "{stdout}");
    assert!(stdout.contains("src/billing/stripe.ts"), "{stdout}");
    assert!(stdout.contains("tests/billing.test.ts"), "{stdout}");
    assert!(!stdout.contains("docs/stripe-notes.md"), "{stdout}");
    assert!(!stdout.contains("vendor/stripe-sdk.js"), "{stdout}");
    assert!(
        !stdout.contains("generated/schema.generated.ts"),
        "{stdout}"
    );
}

#[test]
fn unsupported_find_predicates_pass_through() {
    let cwd = fixture();
    let output = run_agentgrep(
        &cwd,
        &[
            "run",
            "find . -path './vendor' -prune -o -type f -print",
            "--limit",
            "50",
            "--budget",
            "4000",
        ],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "{stdout}");
    assert!(!stdout.contains("agentgrep optimized:"), "{stdout}");
    assert!(stdout.contains("./src/billing/stripe.ts"), "{stdout}");
}

#[test]
fn shims_install_status_and_uninstall() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("shims");
    let dir_arg = dir.to_string_lossy().to_string();

    let installed = run_agentgrep(tmp.path(), &["shims", "install", "--dir", &dir_arg]);
    let install_stdout = String::from_utf8_lossy(&installed.stdout);
    assert!(installed.status.success());
    assert!(install_stdout.contains("installed: 28"));
    assert!(dir.join("rg").is_file());

    let status = run_agentgrep(tmp.path(), &["shims", "status", "--dir", &dir_arg]);
    let status_stdout = String::from_utf8_lossy(&status.stdout);
    assert!(status.status.success());
    assert!(status_stdout.contains("rg: installed"));
    assert!(status_stdout.contains("installed: 28/28"));

    let uninstalled = run_agentgrep(tmp.path(), &["shims", "uninstall", "--dir", &dir_arg]);
    assert!(uninstalled.status.success());
    assert!(!dir.join("rg").exists());
}

#[test]
fn shims_default_to_user_local_bin() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let home_arg = home.to_string_lossy().to_string();

    let installed =
        run_agentgrep_with_env(tmp.path(), &["shims", "install"], &[("HOME", &home_arg)]);
    let install_stdout = String::from_utf8_lossy(&installed.stdout);
    assert!(installed.status.success(), "{install_stdout}");
    assert!(install_stdout.contains(&format!("dir: {}/.local/bin", home.display())));
    assert!(home.join(".local/bin/rg").is_file());

    let status = run_agentgrep_with_env(tmp.path(), &["shims", "status"], &[("HOME", &home_arg)]);
    let status_stdout = String::from_utf8_lossy(&status.stdout);
    assert!(status.status.success(), "{status_stdout}");
    assert!(status_stdout.contains("rg: installed"));
    assert!(status_stdout.contains("installed: 28/28"));
}

#[cfg(unix)]
#[test]
fn shims_status_reports_shadowed_path_order() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("shims");
    let dir_arg = dir.to_string_lossy().to_string();
    let fake_bin = tmp.path().join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let fake_find = fake_bin.join("find");
    fs::write(&fake_find, "#!/bin/sh\nexit 0\n").unwrap();
    let mut permissions = fs::metadata(&fake_find).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_find, permissions).unwrap();

    let installed = run_agentgrep(tmp.path(), &["shims", "install", "--dir", &dir_arg]);
    assert!(installed.status.success());

    let path = format!(
        "{}:{}:{}",
        fake_bin.display(),
        dir.display(),
        env::var("PATH").unwrap_or_default()
    );
    let status = run_agentgrep_with_env(
        tmp.path(),
        &["shims", "status", "--dir", &dir_arg],
        &[("PATH", &path)],
    );
    let stdout = String::from_utf8_lossy(&status.stdout);

    assert!(status.status.success(), "{stdout}");
    assert!(stdout.contains("PATH: present but shadowed"), "{stdout}");
    assert!(stdout.contains("find: installed (shadowed by"), "{stdout}");
}

#[test]
fn rg_shim_proxies_without_recursing() {
    if !has_command("rg") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("shims");
    let dir_arg = dir.to_string_lossy().to_string();
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).unwrap();
    let content = iter::repeat_n("needle in a haystack\n", 1200).collect::<String>();
    fs::write(cwd.join("huge.txt"), content).unwrap();

    let installed = run_agentgrep(tmp.path(), &["shims", "install", "--dir", &dir_arg]);
    assert!(installed.status.success());

    let path = format!("{}:{}", dir.display(), env::var("PATH").unwrap_or_default());
    let output = Command::new("sh")
        .arg("-c")
        .arg("rg needle")
        .current_dir(&cwd)
        .env("PATH", path)
        .env("AGENTGREP_TEE", "0")
        .env("AGENTGREP_LIMIT", "2")
        .output()
        .expect("rg shim runs");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(stdout.contains("agentgrep optimized: rg needle"));
    assert!(stdout.contains("Showing 2."));
    assert!(stdout.contains("huge.txt:"));
    assert!(stdout.contains("Exit code: 0"));
}

#[test]
fn rg_shim_preserves_stdin_pipeline() {
    if !has_command("rg") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("shims");
    let dir_arg = dir.to_string_lossy().to_string();
    let installed = run_agentgrep(tmp.path(), &["shims", "install", "--dir", &dir_arg]);
    assert!(installed.status.success());

    let path = format!("{}:{}", dir.display(), env::var("PATH").unwrap_or_default());
    let output = Command::new("sh")
        .arg("-c")
        .arg("printf 'needle\\nnope\\n' | rg needle")
        .env("PATH", path)
        .output()
        .expect("rg stdin shim runs");

    assert!(output.status.success());
    assert_eq!(output.stdout, b"needle\n");
    assert!(output.stderr.is_empty());
}

#[test]
fn rg_shim_raw_env_matches_real_output() {
    if !has_command("rg") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("shims");
    let dir_arg = dir.to_string_lossy().to_string();
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).unwrap();
    fs::write(cwd.join("sample.txt"), "needle\nnope\n").unwrap();

    let installed = run_agentgrep(tmp.path(), &["shims", "install", "--dir", &dir_arg]);
    assert!(installed.status.success());

    let path = format!("{}:{}", dir.display(), env::var("PATH").unwrap_or_default());
    let output = Command::new("sh")
        .arg("-c")
        .arg("rg needle")
        .current_dir(&cwd)
        .env("PATH", path)
        .env("AGENTGREP_RAW", "1")
        .output()
        .expect("raw rg shim runs");

    assert!(output.status.success());
    assert_eq!(output.stdout, b"sample.txt:needle\n");
    assert!(output.stderr.is_empty());
}

#[test]
fn listing_shims_use_filtered_map_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("shims");
    let dir_arg = dir.to_string_lossy().to_string();
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(cwd.join("src")).unwrap();
    fs::create_dir_all(cwd.join("vendor")).unwrap();
    fs::create_dir_all(cwd.join("generated")).unwrap();
    fs::write(cwd.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(cwd.join("vendor/sdk.js"), "generated vendor code\n").unwrap();
    fs::write(
        cwd.join("generated/schema.generated.ts"),
        "type Generated = {}\n",
    )
    .unwrap();

    let installed = run_agentgrep(tmp.path(), &["shims", "install", "--dir", &dir_arg]);
    assert!(installed.status.success());

    let path = format!("{}:{}", dir.display(), env::var("PATH").unwrap_or_default());
    for command in ["find . -type f", "ls -R"] {
        let output = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&cwd)
            .env("PATH", &path)
            .output()
            .expect("listing shim runs");
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(output.status.success(), "{command}: {stdout}");
        assert!(
            stdout.contains("agentgrep optimized:"),
            "{command}: {stdout}"
        );
        assert!(stdout.contains("src/main.rs"), "{command}: {stdout}");
        assert!(!stdout.contains("vendor/sdk.js"), "{command}: {stdout}");
        assert!(
            !stdout.contains("generated/schema.generated.ts"),
            "{command}: {stdout}"
        );
    }
}

#[test]
fn listing_shims_preserve_shell_pipeline_streams() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("shims");
    let dir_arg = dir.to_string_lossy().to_string();
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(cwd.join("src")).unwrap();
    fs::create_dir_all(cwd.join("vendor")).unwrap();
    fs::write(cwd.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(cwd.join("vendor/sdk.rs"), "vendor\n").unwrap();

    let installed = run_agentgrep(tmp.path(), &["shims", "install", "--dir", &dir_arg]);
    assert!(installed.status.success());

    let path = format!("{}:{}", dir.display(), env::var("PATH").unwrap_or_default());
    let output = Command::new("sh")
        .arg("-c")
        .arg("find . -type f -name '*.rs' | head -1")
        .current_dir(&cwd)
        .env("PATH", &path)
        .output()
        .expect("listing pipeline runs");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "{stdout}");
    assert!(!stdout.contains("agentgrep optimized:"), "{stdout}");
    assert!(stdout.starts_with("./"), "{stdout}");
}

#[test]
fn search_shims_preserve_shell_command_substitution_streams() {
    if !has_command("rg") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("shims");
    let dir_arg = dir.to_string_lossy().to_string();
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).unwrap();
    let content = iter::repeat_n("needle in a haystack\n", 1200).collect::<String>();
    fs::write(cwd.join("huge.txt"), content).unwrap();

    let installed = run_agentgrep(tmp.path(), &["shims", "install", "--dir", &dir_arg]);
    assert!(installed.status.success());

    let path = format!("{}:{}", dir.display(), env::var("PATH").unwrap_or_default());
    let output = Command::new("sh")
        .arg("-c")
        .arg("files=$(rg -l needle); printf '%s\\n' \"$files\"")
        .current_dir(&cwd)
        .env("PATH", &path)
        .env("AGENTGREP_LIMIT", "2")
        .output()
        .expect("command substitution runs");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "{stdout}");
    assert!(!stdout.contains("agentgrep optimized:"), "{stdout}");
    assert_eq!(stdout.trim(), "huge.txt");
}

#[test]
fn missing_file_reads_bypass_active_shims_for_exact_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("shims");
    let dir_arg = dir.to_string_lossy().to_string();

    let installed = run_agentgrep(tmp.path(), &["shims", "install", "--dir", &dir_arg]);
    assert!(installed.status.success());

    let path = format!("{}:{}", dir.display(), env::var("PATH").unwrap_or_default());
    let output = run_agentgrep_with_env(
        tmp.path(),
        &["run", "head -50 missing-file.txt"],
        &[("PATH", &path)],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(stdout.is_empty(), "{stdout}");
    assert!(!stderr.is_empty());
    assert!(!stderr.contains("agentgrep"));
}

#[test]
fn run_raw_bypasses_active_shims() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("shims");
    let dir_arg = dir.to_string_lossy().to_string();
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(cwd.join("src")).unwrap();
    fs::create_dir_all(cwd.join("vendor")).unwrap();
    fs::write(cwd.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(cwd.join("vendor/sdk.js"), "vendor\n").unwrap();

    let installed = run_agentgrep(tmp.path(), &["shims", "install", "--dir", &dir_arg]);
    assert!(installed.status.success());

    let path = format!("{}:{}", dir.display(), env::var("PATH").unwrap_or_default());
    let output = run_agentgrep_with_env(
        &cwd,
        &["run", "find . -type f", "--raw"],
        &[("PATH", &path)],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "{stdout}");
    assert!(!stdout.contains("agentgrep optimized:"), "{stdout}");
    assert!(stdout.contains("./src/main.rs"), "{stdout}");
    assert!(stdout.contains("./vendor/sdk.js"), "{stdout}");
}

#[test]
fn large_file_is_summarized_and_raw_is_exact() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("large.rs");
    let mut content = String::from("use std::fmt;\n\npub struct StripeThing;\n");
    for i in 0..320 {
        content.push_str(&format!("pub fn stripe_{i}() -> usize {{ {i} }}\n"));
    }
    fs::write(&file, &content).unwrap();

    let compact = run_agentgrep(tmp.path(), &["file", "large.rs"]);
    let compact_stdout = String::from_utf8_lossy(&compact.stdout);
    assert!(compact.status.success());
    assert!(compact_stdout.contains("Summary mode"));
    assert!(compact_stdout.contains("Truncated:"));

    let raw = run_agentgrep(tmp.path(), &["file", "large.rs", "--raw"]);
    assert_eq!(raw.stdout, content.as_bytes());
    assert!(raw.stderr.is_empty());
}

#[test]
fn git_status_is_compacted_but_git_mutation_parser_is_covered_by_unit_tests() {
    if !has_command("git") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    Command::new("git")
        .arg("init")
        .current_dir(tmp.path())
        .output()
        .expect("git init runs");
    fs::write(tmp.path().join("tracked.txt"), "hello\n").unwrap();

    let output = run_agentgrep(tmp.path(), &["run", "git status", "--budget", "4000"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(stdout.contains("agentgrep optimized: git status"));
    assert!(stdout.contains("? Untracked: 1 file(s)"), "{stdout}");
    assert!(stdout.contains("Exit code: 0"));
    assert!(!stdout.contains("(use \"git add"));
}

#[test]
fn small_exact_reads_passthrough_without_compaction() {
    let cwd = fixture();
    let cat = run_agentgrep(&cwd, &["run", "cat src/billing/stripe.ts"]);
    let raw = fs::read(cwd.join("src/billing/stripe.ts")).unwrap();
    assert!(cat.status.success());
    assert_eq!(cat.stdout, raw);

    let head = run_agentgrep(&cwd, &["run", "head -n 5 docs/stripe-notes.md"]);
    let expected = Command::new("head")
        .args(["-n", "5", "docs/stripe-notes.md"])
        .current_dir(&cwd)
        .output()
        .expect("head runs");
    assert_eq!(head.stdout, expected.stdout);
    assert_eq!(head.stderr, expected.stderr);
    assert_eq!(head.status.code(), expected.status.code());
}

#[test]
fn trace_record_summary_and_replay_work_end_to_end() {
    let cwd = fixture();
    let tmp = tempfile::tempdir().unwrap();
    let trace = tmp.path().join("commands.jsonl");
    let trace_path = trace.to_string_lossy().to_string();

    let recorded = run_agentgrep(
        &cwd,
        &["run", "cat src/billing/stripe.ts", "--trace", &trace_path],
    );
    assert!(recorded.status.success());
    assert!(trace.exists());
    let trace_content = fs::read_to_string(&trace).unwrap();
    assert!(trace_content.contains("\"command\":\"cat src/billing/stripe.ts\""));
    assert!(trace_content.contains("\"family\":\"cat\""));
    assert!(!trace_content.contains("stripeMode"));

    let summary = run_agentgrep(tmp.path(), &["trace", "summary", &trace_path]);
    let summary_stdout = String::from_utf8_lossy(&summary.stdout);
    assert!(summary.status.success());
    assert!(summary_stdout.contains("agentgrep trace summary"));
    assert!(summary_stdout.contains("cat src/billing/stripe.ts"));

    let replay = run_agentgrep(
        tmp.path(),
        &[
            "trace",
            "replay",
            &trace_path,
            "--repo",
            cwd.to_str().unwrap(),
            "--commands",
            "1",
            "--compare",
            "raw,proxy",
        ],
    );
    let replay_stdout = String::from_utf8_lossy(&replay.stdout);
    assert!(replay.status.success(), "{replay_stdout}");
    assert!(replay_stdout.contains("agentgrep trace replay"));
    assert!(replay_stdout.contains("cat src/billing/stripe.ts"));
    assert!(replay_stdout.contains("gates: pass"));
}

#[test]
fn trace_replay_skips_unsafe_commands() {
    let cwd = fixture();
    let tmp = tempfile::tempdir().unwrap();
    let trace = tmp.path().join("commands.jsonl");
    fs::write(
        &trace,
        "{\"version\":1,\"source\":\"test\",\"ts\":1,\"cwd\":\".\",\"command\":\"git status\",\"family\":\"git status\"}\n\
         {\"version\":1,\"source\":\"test\",\"ts\":2,\"cwd\":\".\",\"command\":\"git commit -m nope\",\"family\":\"git mutating\"}\n\
         {\"version\":1,\"source\":\"test\",\"ts\":3,\"cwd\":\".\",\"command\":\"rg stripe > out.txt\",\"family\":\"rg\"}\n",
    )
    .unwrap();

    let replay = run_agentgrep(
        tmp.path(),
        &[
            "trace",
            "replay",
            trace.to_str().unwrap(),
            "--repo",
            cwd.to_str().unwrap(),
            "--commands",
            "2",
            "--compare",
            "raw,proxy",
        ],
    );
    let replay_stdout = String::from_utf8_lossy(&replay.stdout);
    assert!(replay.status.success(), "{replay_stdout}");
    assert!(replay_stdout.contains("git status"));
    assert!(replay_stdout.contains("skipped unsafe/unsupported commands: 2"));
    assert!(replay_stdout.contains("mutating git command"));
    assert!(replay_stdout.contains("shell control or redirection"));
}

#[test]
fn trace_import_codex_reads_sqlite_exec_calls() {
    if !has_command("sqlite3") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("logs.sqlite");
    let trace = tmp.path().join("codex.jsonl");
    let args = serde_json::json!({
        "cmd": "./target/release/agentgrep run \"git status\" --limit 80",
        "workdir": tmp.path(),
    })
    .to_string();
    let body = format!(
        "Received message {}",
        serde_json::json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "arguments": args,
                "call_id": "call_1",
                "name": "exec_command",
            }
        })
    );
    let sql = format!(
        "create table logs (id integer primary key autoincrement, ts integer not null, ts_nanos integer not null, level text not null, target text not null, feedback_log_body text, module_path text, file text, line integer, thread_id text, process_uuid text, estimated_bytes integer not null default 0); \
         insert into logs (ts, ts_nanos, level, target, feedback_log_body, thread_id, estimated_bytes) values (1, 0, 'INFO', 'log', '{}', 'thread', 0);",
        body.replace('\'', "''")
    );
    let sqlite = Command::new("sqlite3")
        .arg(&db)
        .arg(sql)
        .output()
        .expect("sqlite3 creates fixture db");
    assert!(sqlite.status.success());

    let output = run_agentgrep(
        tmp.path(),
        &[
            "trace",
            "import-codex",
            "--db",
            db.to_str().unwrap(),
            "--out",
            trace.to_str().unwrap(),
            "--cwd",
            tmp.path().to_str().unwrap(),
            "--rows",
            "20",
        ],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("Imported 1 records, 1 unique commands"));
    let trace_content = fs::read_to_string(trace).unwrap();
    assert!(trace_content.contains("\"command\":\"git status\""));
    assert!(trace_content.contains("\"observed_command\""));
}

#[test]
fn trace_import_codex_reconstructs_streamed_arguments() {
    if !has_command("sqlite3") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("logs.sqlite");
    let trace = tmp.path().join("codex-stream.jsonl");
    let args = serde_json::json!({
        "cmd": "find . -type f -name '*.ts'",
        "workdir": tmp.path(),
    })
    .to_string();
    let split = args.len() / 2;
    let (first, second) = args.split_at(split);
    let event_1 = serde_json::json!({
        "type": "response.function_call_arguments.delta",
        "item_id": "fc_stream",
        "output_index": 0,
        "delta": first,
    });
    let event_2 = serde_json::json!({
        "type": "response.function_call_arguments.delta",
        "item_id": "fc_stream",
        "output_index": 0,
        "delta": second,
    });
    let done = serde_json::json!({
        "type": "response.output_item.done",
        "item": {
            "id": "fc_stream",
            "type": "function_call",
            "status": "completed",
            "arguments": "",
            "call_id": "call_stream",
            "name": "exec_command",
        },
        "output_index": 0,
    });
    let sql = format!(
        "create table logs (id integer primary key autoincrement, ts integer not null, ts_nanos integer not null, level text not null, target text not null, feedback_log_body text, module_path text, file text, line integer, thread_id text, process_uuid text, estimated_bytes integer not null default 0); \
         insert into logs (ts, ts_nanos, level, target, feedback_log_body, thread_id, estimated_bytes) values \
         (1, 1, 'INFO', 'log', 'websocket event: {}', 'thread', 0), \
         (1, 2, 'INFO', 'log', 'websocket event: {}', 'thread', 0), \
         (1, 3, 'INFO', 'log', 'websocket event: {}', 'thread', 0);",
        event_1.to_string().replace('\'', "''"),
        event_2.to_string().replace('\'', "''"),
        done.to_string().replace('\'', "''"),
    );
    let sqlite = Command::new("sqlite3")
        .arg(&db)
        .arg(sql)
        .output()
        .expect("sqlite3 creates streamed fixture db");
    assert!(sqlite.status.success());

    let output = run_agentgrep(
        tmp.path(),
        &[
            "trace",
            "import-codex",
            "--db",
            db.to_str().unwrap(),
            "--out",
            trace.to_str().unwrap(),
            "--cwd",
            tmp.path().to_str().unwrap(),
            "--rows",
            "20",
        ],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("Imported 1 records, 1 unique commands"));
    let trace_content = fs::read_to_string(trace).unwrap();
    assert!(trace_content.contains("\"command\":\"find . -type f -name '*.ts'\""));
    assert!(trace_content.contains("\"family\":\"find\""));
}

#[test]
fn trace_import_claude_reads_bash_tool_calls() {
    let tmp = tempfile::tempdir().unwrap();
    let projects = tmp.path().join("claude/projects/repo");
    fs::create_dir_all(&projects).unwrap();
    let log = projects.join("session.jsonl");
    let trace = tmp.path().join("claude.jsonl");
    let row = serde_json::json!({
        "type": "assistant",
        "cwd": tmp.path(),
        "timestamp": "2026-04-27T12:00:00.000Z",
        "sessionId": "session",
        "message": {
            "content": [{
                "type": "tool_use",
                "name": "Bash",
                "input": {
                    "command": "grep -rn stripe src",
                    "description": "Search source",
                }
            }]
        }
    });
    fs::write(&log, format!("{}\n", row)).unwrap();

    let output = run_agentgrep(
        tmp.path(),
        &[
            "trace",
            "import-claude",
            "--dir",
            tmp.path().join("claude/projects").to_str().unwrap(),
            "--out",
            trace.to_str().unwrap(),
            "--cwd",
            tmp.path().to_str().unwrap(),
            "--rows",
            "20",
        ],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("Imported 1 records, 1 unique commands"));
    let trace_content = fs::read_to_string(trace).unwrap();
    assert!(trace_content.contains("\"source\":\"claude-jsonl\""));
    assert!(trace_content.contains("\"command\":\"grep -rn stripe src\""));
    assert!(trace_content.contains("\"family\":\"grep\""));
}

#[test]
fn benchmark_reports_required_metrics() {
    if !has_command("rg") {
        return;
    }
    let cwd = fixture();
    let output = run_agentgrep(
        &cwd,
        &[
            "bench",
            "--command",
            "rg --sort path stripe",
            "--compare",
            "raw,proxy,indexed",
        ],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(stdout.contains("agentgrep bench: rg --sort path stripe"));
    assert!(stdout.contains("tokens"));
    assert!(stdout.contains("savings"));
    assert!(stdout.contains("speedup"));
    assert!(stdout.contains("--raw exactness: yes"));
    assert!(stdout.contains("exit-code parity"));
    assert!(stdout.contains("stderr parity"));
}

#[test]
fn benchmark_suite_reports_multiple_discovery_commands() {
    if !has_command("rg") {
        return;
    }
    let cwd = fixture();
    let output = run_agentgrep(
        &cwd,
        &[
            "bench",
            "--suite",
            "discovery",
            "--compare",
            "raw,proxy,indexed",
        ],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(stdout.contains("agentgrep bench suite: discovery"));
    assert!(stdout.contains("rg --sort path stripe"));
    assert!(stdout.contains("head -n 40 docs/stripe-notes.md"));
    assert!(stdout.contains("wc -l docs/stripe-notes.md"));
    assert!(stdout.contains("gates:"));
}

#[test]
fn gain_tracking_uses_sqlite_by_default_path_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("tracking.sqlite");
    let db_arg = db.to_string_lossy().to_string();
    let output = run_agentgrep_with_env(
        tmp.path(),
        &["run", "printf hello"],
        &[("AGENTGREP_TRACKING_PATH", &db_arg)],
    );
    assert!(output.status.success());
    assert!(db.is_file());

    let gain = run_agentgrep(tmp.path(), &["gain", "--path", &db_arg]);
    let stdout = String::from_utf8_lossy(&gain.stdout);

    assert!(gain.status.success(), "{stdout}");
    assert!(stdout.contains("Ledger"), "{stdout}");
    assert!(stdout.contains("Records      1"), "{stdout}");
    assert!(stdout.contains("Command Types"), "{stdout}");
    assert!(stdout.contains("Projects"), "{stdout}");
    assert!(stdout.contains("printf"), "{stdout}");
    assert!(!stdout.contains("printf hello"), "{stdout}");
}

#[test]
fn gain_tracking_defaults_to_user_level_sqlite_db() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let work = tmp.path().join("repo");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&work).unwrap();

    let output = run_agentgrep_with_env(
        &work,
        &["run", "printf hello"],
        &[
            ("HOME", home.to_str().unwrap()),
            ("AGENTGREP_TRACKING", "1"),
        ],
    );
    assert!(output.status.success());
    let db = home.join(".agentgrep/tracking.sqlite");
    assert!(db.is_file());
    assert!(!work.join(".agentgrep/tracking.sqlite").exists());

    let gain = run_agentgrep_with_env(
        &work,
        &["gain"],
        &[
            ("HOME", home.to_str().unwrap()),
            ("AGENTGREP_TRACKING", "1"),
        ],
    );
    let stdout = String::from_utf8_lossy(&gain.stdout);
    assert!(gain.status.success(), "{stdout}");
    assert!(
        stdout.contains(&format!("Ledger       {}", db.display())),
        "{stdout}"
    );
    assert!(stdout.contains("Projects"), "{stdout}");
    assert!(stdout.contains("repo"), "{stdout}");
}

#[test]
fn file_slice_commands_are_compacted_with_line_numbers() {
    let cwd = fixture();
    for command in [
        "head -n 40 docs/stripe-notes.md",
        "tail -n 40 docs/stripe-notes.md",
        "sed -n '1,40p' docs/stripe-notes.md",
        "nl -ba docs/stripe-notes.md | sed -n '1,40p'",
    ] {
        let output = run_agentgrep(&cwd, &["run", command, "--budget", "120"]);
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(output.status.success(), "{command}");
        assert!(
            stdout.contains("agentgrep optimized:"),
            "{command}: {stdout}"
        );
        assert!(
            stdout.contains("| stripe subscription path"),
            "{command}: {stdout}"
        );
        assert!(stdout.contains("Exit code: 0"), "{command}: {stdout}");
    }
}

#[test]
fn wc_and_tree_commands_are_compacted() {
    let cwd = fixture();
    let wc = run_agentgrep(&cwd, &["run", "wc -l docs/stripe-notes.md"]);
    let wc_stdout = String::from_utf8_lossy(&wc.stdout);
    assert!(wc.status.success());
    assert!(wc_stdout.contains("docs/stripe-notes.md"));
    assert!(wc_stdout.contains("62"));

    let tree = run_agentgrep(&cwd, &["run", "tree -L 2 .", "--budget", "200"]);
    let tree_stdout = String::from_utf8_lossy(&tree.stdout);
    assert!(tree.status.success());
    assert!(tree_stdout.contains("src"));
}

#[test]
fn git_grep_and_git_tree_are_compacted() {
    if !has_command("git") {
        return;
    }
    let cwd = repo_root();
    let grep = run_agentgrep(
        &cwd,
        &["run", "git grep agentgrep -- README.md", "--budget", "50"],
    );
    let grep_stdout = String::from_utf8_lossy(&grep.stdout);
    assert!(grep.status.success());
    assert!(grep_stdout.contains("agentgrep optimized: git grep agentgrep -- README.md"));
    assert!(grep_stdout.contains("README.md:"));
    assert!(grep_stdout.contains("Exit code: 0"));

    let tree = run_agentgrep(
        &cwd,
        &[
            "run",
            "git ls-tree -r --name-only HEAD",
            "--limit",
            "5",
            "--budget",
            "50",
        ],
    );
    let tree_stdout = String::from_utf8_lossy(&tree.stdout);
    assert!(tree.status.success());
    assert!(tree_stdout.contains("agentgrep optimized: git ls-tree -r --name-only HEAD"));
    assert!(tree_stdout.contains("git ls-tree:"));
    assert!(tree_stdout.contains("Truncated:"));
}

#[test]
fn hooks_install_claude_writes_project_settings() {
    let tmp = tempfile::tempdir().unwrap();
    let output = run_agentgrep(
        tmp.path(),
        &[
            "hooks",
            "install-claude",
            "--scope",
            "project",
            "--agentgrep",
            "/tmp/agentgrep",
        ],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("Action    added"), "{stdout}");
    assert!(stdout.contains("Installed handler"), "{stdout}");
    assert!(stdout.contains("Undo"), "{stdout}");
    let settings = fs::read_to_string(tmp.path().join(".claude/settings.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&settings).unwrap();
    let hooks = value["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(hooks[0]["matcher"], "Bash");
    assert_eq!(
        hooks[0]["hooks"][0]["command"],
        "/tmp/agentgrep hooks claude-pre-tool-use"
    );
}

#[test]
fn hooks_install_codex_writes_hooks_and_feature_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let output = run_agentgrep(
        tmp.path(),
        &[
            "hooks",
            "install-codex",
            "--scope",
            "project",
            "--agentgrep",
            "/tmp/agentgrep",
        ],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(
        stdout.contains("Feature   created codex_hooks = true"),
        "{stdout}"
    );
    assert!(stdout.contains("Installed handlers"), "{stdout}");
    assert!(
        stdout.contains("Current Codex hooks do not apply"),
        "{stdout}"
    );
    let hooks = fs::read_to_string(tmp.path().join(".codex/hooks.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&hooks).unwrap();
    assert_eq!(value["hooks"]["PreToolUse"][0]["matcher"], "^Bash$");
    assert_eq!(
        value["hooks"]["SessionStart"][0]["hooks"][0]["command"],
        "/tmp/agentgrep hooks codex-session-start"
    );
    let config = fs::read_to_string(tmp.path().join(".codex/config.toml")).unwrap();
    assert!(
        config.contains("[features]\ncodex_hooks = true"),
        "{config}"
    );
}

#[test]
fn hooks_uninstall_claude_removes_only_agentgrep_hooks() {
    let tmp = tempfile::tempdir().unwrap();
    let settings_path = tmp.path().join(".claude/settings.json");
    fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    fs::write(
        &settings_path,
        serde_json::json!({
            "theme": "kept",
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Bash",
                        "hooks": [
                            { "type": "command", "command": "/tmp/agentgrep hooks claude-pre-tool-use" },
                            { "type": "command", "command": "/tmp/other-hook" }
                        ]
                    }
                ]
            }
        })
        .to_string(),
    )
    .unwrap();

    let output = run_agentgrep(
        tmp.path(),
        &["hooks", "uninstall-claude", "--scope", "project"],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("Removed    1 handler"), "{stdout}");
    assert!(
        stdout.contains("Pruned     0 empty groups, 0 empty events"),
        "{stdout}"
    );
    assert!(stdout.contains("Preserved"), "{stdout}");
    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(settings_path).unwrap()).unwrap();
    assert_eq!(value["theme"], "kept");
    assert_eq!(
        value["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "/tmp/other-hook"
    );
}

#[test]
fn hooks_uninstall_codex_removes_only_agentgrep_hooks_and_keeps_feature_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let hooks_path = tmp.path().join(".codex/hooks.json");
    let config_path = tmp.path().join(".codex/config.toml");
    fs::create_dir_all(hooks_path.parent().unwrap()).unwrap();
    fs::write(&config_path, "[features]\ncodex_hooks = true\n").unwrap();
    fs::write(
        &hooks_path,
        serde_json::json!({
            "hooks": {
                "SessionStart": [
                    {
                        "matcher": "startup|resume",
                        "hooks": [
                            { "type": "command", "command": "/tmp/agentgrep hooks codex-session-start" },
                            { "type": "command", "command": "/tmp/keep-session" }
                        ]
                    }
                ],
                "PreToolUse": [
                    {
                        "matcher": "^Bash$",
                        "hooks": [
                            { "type": "command", "command": "/tmp/agentgrep hooks codex-pre-tool-use" }
                        ]
                    }
                ]
            }
        })
        .to_string(),
    )
    .unwrap();

    let output = run_agentgrep(
        tmp.path(),
        &["hooks", "uninstall-codex", "--scope", "project"],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("Removed    2 handlers"), "{stdout}");
    assert!(
        stdout.contains("Pruned     1 empty group, 1 empty event"),
        "{stdout}"
    );
    assert!(stdout.contains("config.toml was not changed"), "{stdout}");
    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(hooks_path).unwrap()).unwrap();
    assert_eq!(
        value["hooks"]["SessionStart"][0]["hooks"][0]["command"],
        "/tmp/keep-session"
    );
    assert!(value["hooks"].get("PreToolUse").is_none());
    assert!(
        fs::read_to_string(config_path)
            .unwrap()
            .contains("codex_hooks = true")
    );
}

#[test]
fn claude_pre_tool_use_rewrites_safe_bash_and_preserves_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let input = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {
            "command": "rg stripe",
            "description": "Search stripe",
            "timeout": 120000
        }
    });
    let output = run_agentgrep_with_stdin(
        tmp.path(),
        &["hooks", "claude-pre-tool-use"],
        &input.to_string(),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let hook_output = &value["hookSpecificOutput"];
    assert_eq!(hook_output["hookEventName"], "PreToolUse");
    assert_eq!(hook_output["permissionDecision"], "allow");
    assert_eq!(
        hook_output["updatedInput"]["command"],
        "agentgrep run 'rg stripe'"
    );
    assert_eq!(hook_output["updatedInput"]["description"], "Search stripe");
    assert_eq!(hook_output["updatedInput"]["timeout"], 120000);
}
