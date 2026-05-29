# Agentgrep MVP Acceptance Criteria

Source of truth for the public repo: this acceptance summary and the current implementation.

## Implemented

- Rust CLI named `agentgrep`.
- `agentgrep run "<command>"` proxy surface.
- Detection for `rg <pattern>`, `grep -R <pattern> .`, `find . -type f`, supported `find -name`/`-iname`/`-maxdepth`/`-mindepth` forms, `ls -R`, `cat <file>`, and read-only `git status`, `git diff`, `git log`, `git show`, `git branch`, `git ls-files`.
- Additional agent-habit intercepts: `head`, `tail`, numeric `sed -n`, `nl -ba ... | sed -n ...`, `wc -l`, `tree`, `git grep`, `git ls-tree`, `cargo test/check/clippy`, `pytest`, `python -m pytest`, `go test`, npm/pnpm/yarn/Bun test scripts, Vitest, Jest, Playwright, Ruff, Mypy, `deps`, and small read-only git inspect commands.
- Benchmark-suite coverage for expanded command families where repo signals exist: `cargo check`, `cargo clippy`, `cargo test`, pytest collection, `go test`, npm/pnpm/yarn test scripts, Vitest, Jest, Playwright, Ruff, and Mypy.
- Direct commands: `regex`, `file`, `map`, `deps`, `index`, `bench`, `trace`, `gain`, `mcp`, `shims`, `doctor`.
- MCP stdio server: `agentgrep mcp [root]` exposes `agentgrep_status`, `agentgrep_schema`, `agentgrep_search`, `agentgrep_context`, `agentgrep_file`, `agentgrep_map`, `agentgrep_outline`, `agentgrep_symbol`, `agentgrep_callers`, `agentgrep_deps`, and safe `agentgrep_run` tools with JSON schemas, root-scoped path validation, and structured JSON-RPC errors for unknown tools, bad arguments, path escapes, mutating git, unsupported commands, and unsafe shell syntax.
- MCP builds a cached source index on startup, refreshes it when the root file set changes, and uses it for structural outline, symbol, caller, and dependency-edge tools.
- MCP rejects sensitive local paths such as `.env*`, credentials, secrets, private keys, `.ssh`, and `.aws` even when they are inside the root.
- Opt-in shims can proxy `rg`, `grep`, `find`, `ls`, `cat`, `git`, `head`, `tail`, `sed`, `nl`, `wc`, `tree`, `cargo`, `pytest`, `py.test`, `python`, `python3`, `go`, `npm`, `pnpm`, `yarn`, `bun`, `bunx`, `npx`, `vitest`, `jest`, `playwright`, `ruff`, `mypy`, and `deps` without requiring agents to change command habits.
- Shims preserve stdin and shell pipeline/redirection stream semantics by declining optimization when the parent shell command is composite.
- Compact exact search output with file path, line number, matched line, nearby context, truncation notice, and raw fallback hint.
- Large-file summaries by default for `file` and proxied `cat`; `--raw` emits exact bytes.
- Repo maps hide ignored/generated/vendor/build/dependency/binary/lock files and honor supported filtered `find` predicates.
- Read-only git commands are compacted conservatively while preserving exit code and stderr; mutating git commands pass through unchanged.
- Plain/verbose `git log` uses a compact format that keeps commit hashes, subjects, relative dates, authors, and selected body lines. Explicit user log formats are passed through or compacted only after raw capture.
- Test-runner commands compact large stdout only while preserving stderr byte-for-byte in V1.
- Optimized raw probes use streaming capture and cap stored stdout by `AGENTGREP_CAPTURE_MAX_STDOUT_BYTES` by default; cap notices are explicit and `--raw` remains byte-for-byte exact.
- `deps` summarizes dependency manifests for Rust, Node, Python, and Go projects.
- Unsupported shimmed command families and mutating git passthrough bypass active agentgrep shim directories before invoking the real tool.
- Small raw outputs pass through exactly by default; compaction only applies when raw output exceeds the budget or a direct compact command is used.
- Truncated optimized proxy output can tee full raw output under `.agentgrep/tee`, with `AGENTGREP_TEE=0` as an escape hatch.
- `AGENTGREP_DISABLE=1` bypasses optimization for `agentgrep run`.
- Output controls: `--raw`, `--json`, `--exact`, `--limit`, `--budget`.
- Bench command: `agentgrep bench --command 'rg stripe' --compare raw,proxy,indexed`.
- Benchmark suites: `agentgrep bench --suite discovery --compare raw,proxy,indexed`, workspace-local all-family coverage with `agentgrep bench --suite all --compare raw,proxy,indexed`, and optional codedb-facing parity coverage with `agentgrep bench --suite agent-dx --compare raw,proxy,indexed,codedb`.
- Benchmark metrics: time, bytes, estimated tokens, token savings, speedup, exit-code parity, stderr parity, and `--raw` exactness.
- Coverage readiness is reported by `agentgrep doctor` via an optional `cargo llvm-cov` check; the coverage command is `cargo llvm-cov --all-targets --workspace --summary-only`.
- RTK-derived runtime shape: streaming raw capture before compaction, parser tiers that decline unsafe or unsupported forms, exact small-output fallback, explicit optimized-capture cap notices, capped and rotated tee recovery for truncated stdout, persistent metadata-only SQLite gain records with per-command and overall savings summaries, and incremental benchmark coverage for expanded runner families.
- Trace recording: `agentgrep run "<command>" --trace <path>` and `AGENTGREP_TRACE=<path>`.
- Trace dogfooding: `agentgrep trace import-codex`, `agentgrep trace import-claude`, `agentgrep trace summary`, and `agentgrep trace replay`.
- Codex trace import reconstructs streamed function-call argument deltas and stores command metadata only.
- Claude trace import reads Bash tool-call command metadata from local project JSONL logs under the requested cwd subtree.
- Trace replay benchmarks safe read-only discovery commands and skips mutating/unsupported/shell-control commands with reasons.

## V1 constraints

- The disk index is lightweight JSON metadata plus trigram summaries. MCP also maintains an in-process structural source index for warm-session tool calls, but it is not a full persistent mmap search engine.
- Proxy search executes the raw command first to preserve stderr and exit-code behavior, then renders compact output. This prioritizes safety over maximum speed in the first version.
- Tree-sitter-grade symbol precision, remote-repo querying, atomic edit tools, and semantic search are later phases from the PRD.
