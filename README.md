# agentgrep

`agentgrep` is a local Rust CLI and command proxy for coding agents. It lets agents keep using familiar shell discovery commands like `rg`, `grep -R`, `find`, `ls -R`, `cat`, and read-only `git` commands, while returning compact, exact, token-bounded output by default.

The product principle is simple: do not fight the agent's shell habits. Make the commands agents already emit cheaper and safer underneath them.

## Install locally

```bash
cargo build --release
./target/release/agentgrep doctor
```

## Proxy commands

```bash
agentgrep run "rg stripe"
agentgrep run "grep -R stripe ."
agentgrep run "find . -type f"
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
agentgrep run "git grep createSubscription -- src"
agentgrep run "git ls-tree -r --name-only HEAD"
```

Unsupported commands and mutating `git` commands are passed through unchanged in V1. Use `--raw` for exact passthrough output:

```bash
agentgrep run "rg stripe" --raw
```

Safety defaults:

- Small outputs that fit the current `--budget` pass through exactly.
- Explicit bounded reads like `head`, `tail`, `sed -n`, and small `cat` calls stay raw unless they exceed the budget.
- Compacted truncated output includes a raw rerun hint, and when raw output is large enough it is tee'd under `.agentgrep/tee`.
- Set `AGENTGREP_DISABLE=1` to bypass proxy optimization for `agentgrep run`.
- Set `AGENTGREP_TEE=0` to disable full-output tee files.

## Direct commands

```bash
agentgrep regex "createSubscription\\("
agentgrep regex "stripe" --exact
agentgrep file src/billing/stripe.ts
agentgrep file src/billing/stripe.ts --lines 72:112
agentgrep map .
agentgrep index .
agentgrep bench --command 'rg stripe' --compare raw,proxy,indexed
agentgrep bench --suite discovery --compare raw,proxy,indexed
agentgrep doctor
```

## Traces

Trace recording lets `agentgrep` learn the commands agents actually emit without capturing command output:

```bash
agentgrep run "rg stripe" --trace .agentgrep/traces/commands.jsonl
AGENTGREP_TRACE=.agentgrep/traces/commands.jsonl agentgrep run "git status"
agentgrep trace import-codex --out .agentgrep/traces/codex.jsonl
agentgrep trace summary .agentgrep/traces/codex.jsonl
agentgrep trace replay .agentgrep/traces/codex.jsonl --repo . --compare raw,proxy,indexed
```

Trace JSONL records command metadata only: command, cwd, family, timing, exit code, and output byte/token counts. It does not store stdout or stderr. `trace import-codex` reads local Codex `exec_command` calls from `~/.codex/logs_2.sqlite` using `sqlite3`, unwraps dogfooded `agentgrep run "..."` calls back to the underlying command when safe, and writes a replayable JSONL trace. `trace replay` benchmarks only safe read-only discovery commands; mutating `git`, unsupported commands, shell control operators, and redirections are skipped with reasons.

Common output controls:

- `--raw`: exact raw output where applicable.
- `--json`: structured JSON envelope.
- `--exact`: literal text matching for search.
- `--limit`: maximum primary items to show.
- `--budget`: approximate output token budget.

## Safety guarantees in V1

Agentgrep preserves exit codes, stderr, errors, file paths, line numbers, exact matched lines, and explicit truncation notices. It hides ignored/generated/vendor/build/dependency/binary files in repo maps and direct search, and it does not compress mutating `git` commands.

The proxy follows RTK's conservative shape: passthrough when unsupported or unsafe, raw fallback when output is already small, stderr and exit-code parity, and a recovery path for truncated output.

## Benchmarks

The benchmark command compares raw, proxy, and indexed modes:

```bash
agentgrep bench --command 'rg stripe' --compare raw,proxy,indexed
agentgrep bench --suite discovery --compare raw,proxy,indexed
agentgrep trace replay .agentgrep/traces/codex.jsonl --repo .
```

It reports raw/proxy/indexed time, output bytes, estimated tokens, token savings, speedup ratio, exit-code parity, stderr parity, and `--raw` exactness. Gates are reported for raw exactness, exit-code parity, stderr parity, truncation visibility, and 60% token savings when raw output is large enough to matter.

The built-in `discovery` suite replays a small mix of realistic agent reads: broad search, recursive listing, file reads, line slices, and line counts.

Trace replay is the dogfood loop for local workspaces: import the Codex session trace, summarize the command families agents are leaning on, then benchmark the top safe commands against raw/proxy/indexed behavior.
