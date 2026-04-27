# Agentgrep MVP Acceptance Criteria

Source of truth for the public repo: this acceptance summary and the current implementation.

## Implemented

- Rust CLI named `agentgrep`.
- `agentgrep run "<command>"` proxy surface.
- Detection for `rg <pattern>`, `grep -R <pattern> .`, `find . -type f`, `ls -R`, `cat <file>`, and read-only `git status`, `git diff`, `git log`, `git show`, `git branch`, `git ls-files`.
- Additional agent-habit intercepts: `head`, `tail`, numeric `sed -n`, `nl -ba ... | sed -n ...`, `wc -l`, `tree`, `git grep`, `git ls-tree`, and small read-only git inspect commands.
- Direct commands: `regex`, `file`, `map`, `index`, `bench`, `trace`, `shims`, `doctor`.
- Opt-in shims can proxy `rg`, `grep`, `find`, `ls`, `cat`, `git`, `head`, `tail`, `sed`, `nl`, `wc`, and `tree` without requiring agents to change command habits.
- Compact exact search output with file path, line number, matched line, nearby context, truncation notice, and raw fallback hint.
- Large-file summaries by default for `file` and proxied `cat`; `--raw` emits exact bytes.
- Repo maps hide ignored/generated/vendor/build/dependency/binary/lock files.
- Read-only git commands are compacted conservatively while preserving exit code and stderr; mutating git commands pass through unchanged.
- Small raw outputs pass through exactly by default; compaction only applies when raw output exceeds the budget or a direct compact command is used.
- Truncated optimized proxy output can tee full raw output under `.agentgrep/tee`, with `AGENTGREP_TEE=0` as an escape hatch.
- `AGENTGREP_DISABLE=1` bypasses optimization for `agentgrep run`.
- Output controls: `--raw`, `--json`, `--exact`, `--limit`, `--budget`.
- Bench command: `agentgrep bench --command 'rg stripe' --compare raw,proxy,indexed`.
- Benchmark suite: `agentgrep bench --suite discovery --compare raw,proxy,indexed`.
- Benchmark metrics: time, bytes, estimated tokens, token savings, speedup, exit-code parity, stderr parity, and `--raw` exactness.
- Trace recording: `agentgrep run "<command>" --trace <path>` and `AGENTGREP_TRACE=<path>`.
- Trace dogfooding: `agentgrep trace import-codex`, `agentgrep trace summary`, and `agentgrep trace replay`.
- Trace replay benchmarks safe read-only discovery commands and skips mutating/unsupported/shell-control commands with reasons.

## V1 constraints

- The index is lightweight JSON metadata plus trigram summaries. It is enough to support indexing workflow and future candidate filtering, but it is not yet a full persistent search engine.
- Proxy search executes the raw command first to preserve stderr and exit-code behavior, then renders compact output. This prioritizes safety over maximum speed in the first version.
- MCP tools, richer shell hook installers, tree-sitter symbols, and semantic search are later phases from the PRD.
