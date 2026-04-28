# agentgrep

`agentgrep` is a local Rust CLI and command proxy for coding agents. It lets agents keep using familiar shell discovery commands like `rg`, `grep -R`, `find`, `ls -R`, `cat`, and read-only `git` commands, while returning compact, exact, token-bounded output by default.

The product principle is simple: do not fight the agent's shell habits. Make the commands agents already emit cheaper and safer underneath them.

## Install locally

```bash
cargo build --release
./target/release/agentgrep doctor
```

Install command shims when you want familiar commands to proxy through `agentgrep` without changing agent workflows:

```bash
agentgrep shims install --dir ~/.local/bin/agentgrep-shims
export PATH="$HOME/.local/bin/agentgrep-shims:$PATH"
agentgrep shims status --dir ~/.local/bin/agentgrep-shims
```

Shims are available for `rg`, `grep`, `find`, `ls`, `cat`, `git`, `head`, `tail`, `sed`, `nl`, `wc`, `tree`, `cargo`, `pytest`, `py.test`, `python`, `python3`, `go`, `npm`, `pnpm`, `yarn`, `npx`, `vitest`, `jest`, `playwright`, `ruff`, `mypy`, and `deps`. `agentgrep shims status` reports when the shim directory is present but shadowed by earlier system paths. They remove their own directory from `PATH` before executing so raw fallback resolves the real tool instead of recursing, pass piped stdin directly to the real tool, and decline optimization when a parent shell command contains a pipeline or redirection. Remove them with:

```bash
agentgrep shims uninstall --dir ~/.local/bin/agentgrep-shims
```

## Proxy commands

```bash
agentgrep run "rg stripe"
agentgrep run "grep -R stripe ."
agentgrep run "find . -type f"
agentgrep run "find . -type f -name '*.rs' -maxdepth 3"
agentgrep run "ls -R"
agentgrep run "cat src/main.rs"
agentgrep run "head -n 80 src/main.rs"
agentgrep run "tail -n 80 src/main.rs"
agentgrep run "sed -n '72,112p' src/main.rs"
agentgrep run "nl -ba src/main.rs | sed -n '72,112p'"
agentgrep run "wc -l src/main.rs"
agentgrep run "tree -L 2 ."
agentgrep run "git status"
agentgrep run "git diff"
agentgrep run "git log"
agentgrep run "git grep createSubscription -- src"
agentgrep run "git ls-tree -r --name-only HEAD"
agentgrep run "cargo check"
agentgrep run "cargo clippy"
agentgrep run "cargo test -- --list"
agentgrep run "pytest -q"
agentgrep run "python -m pytest -q"
agentgrep run "go test ./..."
agentgrep run "deps"
```

Unsupported commands and mutating `git` commands are passed through unchanged in V1. Use `--raw` for exact passthrough output:

```bash
agentgrep run "rg stripe" --raw
```

Safety defaults:

- Small outputs that fit the current `--budget` pass through exactly.
- Explicit bounded reads like `head`, `tail`, `sed -n`, and small `cat` calls stay raw unless they exceed the budget.
- Repo listing commands like `find . -type f`, supported `find -name`/`-iname`/`-maxdepth`/`-mindepth` forms, `ls -R`, and `tree` use the filtered in-process map by default; use `--raw` for original listings.
- Unsupported `find` predicates such as pruning, boolean expressions, execs, deletes, path regexes, and unknown tests pass through to the real tool for exact semantics.
- Complex `rg`/`grep` forms with filters, sort options, context flags, or `-e` patterns compact the actual raw result stream instead of re-running an approximate search.
- Plain/verbose `git log` is rendered through a compact format that keeps commit hash, subject, relative date, author, and a few body lines; explicit user formats such as `--oneline`, `--pretty`, and `--format` are preserved.
- `cargo test`, `cargo check`, `cargo clippy`, `pytest`, `python -m pytest`, `go test`, Node package-manager test scripts, `vitest`, `jest`, `playwright`, `ruff`, and `mypy` are recognized as runner/diagnostic commands. V1 only compacts large stdout while preserving stderr byte-for-byte, so compile errors and runner diagnostics are not silently hidden.
- `deps` summarizes dependency manifests (`Cargo.toml`, `package.json`, `requirements.txt`, `pyproject.toml`, and `go.mod`) without pretending to be an exact manifest read.
- Compacted truncated output includes a raw rerun hint, and when raw output is large enough it is tee'd under `.agentgrep/tee`.
- Optimized raw probes stream stdout/stderr instead of using one giant `Command::output()` buffer. Stdout capture is capped by `AGENTGREP_CAPTURE_MAX_STDOUT_BYTES` (default 4 MiB, `0` disables) for optimized renderers; `--raw` remains byte-for-byte exact and ignores this cap.
- `--raw`, `AGENTGREP_DISABLE=1`, unsupported shimmed commands, and mutating `git` passthrough all bypass active agentgrep shim directories before running the underlying command.
- Set `AGENTGREP_DISABLE=1` to bypass proxy optimization for `agentgrep run`.
- Set `AGENTGREP_TEE=0` to disable full-output tee files.
- Shims also read `AGENTGREP_LIMIT`, `AGENTGREP_BUDGET`, `AGENTGREP_RAW`, `AGENTGREP_JSON`, and `AGENTGREP_EXACT` for default output behavior.

## Direct commands

```bash
agentgrep regex "createSubscription\\("
agentgrep regex "stripe" --exact
agentgrep file src/billing/stripe.ts
agentgrep file src/billing/stripe.ts --lines 72:112
agentgrep map .
agentgrep index .
agentgrep deps .
agentgrep bench --command 'rg stripe' --compare raw,proxy,indexed
agentgrep bench --suite discovery --compare raw,proxy,indexed
agentgrep bench --suite all --compare raw,proxy,indexed
agentgrep doctor
```

## Traces

Trace recording lets `agentgrep` learn the commands agents actually emit without capturing command output:

```bash
agentgrep run "rg stripe" --trace .agentgrep/traces/commands.jsonl
AGENTGREP_TRACE=.agentgrep/traces/commands.jsonl agentgrep run "git status"
agentgrep trace import-codex --out .agentgrep/traces/codex.jsonl
agentgrep trace import-claude --out .agentgrep/traces/claude.jsonl
agentgrep trace summary .agentgrep/traces/codex.jsonl
agentgrep trace replay .agentgrep/traces/codex.jsonl --repo . --compare raw,proxy,indexed
agentgrep gain
```

Trace JSONL records command metadata only: command, cwd, family, timing, exit code, and output byte/token counts. It does not store stdout or stderr. `trace import-codex` reads local Codex `exec_command` calls from `~/.codex/logs_2.sqlite` using `sqlite3`, reconstructs streamed function-call argument deltas, unwraps dogfooded `agentgrep run "..."` calls back to the underlying command when safe, and writes a replayable JSONL trace. `trace import-claude` reads local Claude project JSONL logs from `~/.claude/projects` and imports Bash tool-call commands under the requested cwd subtree. `trace replay` benchmarks only safe read-only discovery commands; mutating `git`, unsupported commands, shell control operators, and redirections are skipped with reasons.

Persistent gain tracking records metadata only to `.agentgrep/tracking.sqlite` by default: sanitized command, cwd/project, input/output token estimates, saved tokens, savings percentage, and elapsed milliseconds. It never stores stdout or stderr. `agentgrep gain` summarizes per-command, per-project, and overall savings from that SQLite database. Set `AGENTGREP_TRACKING=0` to disable it, or `AGENTGREP_TRACKING_PATH=/path/to/tracking.sqlite` to move the ledger. Legacy `.jsonl` ledgers are still readable and writable when the configured path ends in `.jsonl`.

Common output controls:

- `--raw`: exact raw output where applicable.
- `--json`: structured JSON envelope.
- `--exact`: literal text matching for search.
- `--limit`: maximum primary items to show.
- `--budget`: approximate output token budget.

## Safety guarantees in V1

Agentgrep preserves exit codes, stderr, errors, file paths, line numbers, exact matched lines, and explicit truncation notices. It hides ignored/generated/vendor/build/dependency/binary files in repo maps and direct search, and it does not compress mutating `git` commands.

The proxy follows RTK's conservative shape:

- A conservative streaming runner drains raw stdout, stderr, and exit status before deciding whether compaction is safe; optimized stdout capture is bounded and any cap is disclosed, while `--raw` stays exact.
- Parser tiers recognize high-confidence command families first, decline unsafe shell forms, and fall back to the real tool for unsupported commands.
- Small raw output remains exact; large output is compacted only when the family has a conservative renderer.
- Tee recovery can persist full raw stdout under `.agentgrep/tee` for truncated optimized output, while preserving stderr byte-for-byte.
- Trace and gain recording store command metadata so command-family coverage and savings can be improved without capturing command output.
- Expanded test and check families are staged behind the same pattern: parse narrowly, cap output, preserve diagnostics, and leave honest raw fallback when the renderer is not integrated yet.

## Benchmarks

The benchmark command compares raw, proxy, and indexed modes:

```bash
agentgrep bench --command 'rg stripe' --compare raw,proxy,indexed
agentgrep bench --suite discovery --compare raw,proxy,indexed
agentgrep bench --suite all --compare raw,proxy,indexed
agentgrep trace replay .agentgrep/traces/codex.jsonl --repo .
```

It reports raw/proxy/indexed time, output bytes, estimated tokens, token savings, speedup ratio, exit-code parity, stderr parity, and `--raw` exactness. Gates are reported for raw exactness, exit-code parity, stderr parity, truncation visibility, and 60% token savings when raw output is large enough to matter.

The built-in `discovery` suite replays a small fixture mix of realistic agent reads: broad search, recursive listing, file reads, line slices, and line counts. The `all` suite builds a workspace-local benchmark set that covers every intercepted command family, including read-only git commands when the current repo supports them. It also adds repo-detected coverage placeholders for expanded runner families: Cargo check/clippy/test, pytest, Go tests, Node package-manager tests, Vitest, Jest, Playwright, Ruff, and Mypy when their manifests, configs, scripts, or tools are present.

Trace replay is the dogfood loop for local workspaces: import the Codex session trace, summarize the command families agents are leaning on, then benchmark the top safe commands against raw/proxy/indexed behavior.
