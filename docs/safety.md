# Agentgrep Proxy Safety

Agentgrep should never make an agent miss the thing it explicitly asked a shell command to show.

Safety policy:

- Unsupported commands pass through unchanged, and unsupported shimmed command families bypass active agentgrep shim directories before invoking the real tool.
- Mutating git commands pass through unchanged and bypass active agentgrep shim directories.
- Small command output that fits `--budget` passes through unchanged.
- Explicit bounded reads, such as `head`, `tail`, `sed -n`, and small `cat`, preserve raw output when it fits the budget.
- Repo listing commands, such as `find . -type f`, supported `find -name`/`-iname`/`-maxdepth`/`-mindepth` forms, `ls -R`, and `tree`, use the filtered in-process map by default; use `--raw` for original listings.
- Unsupported `find` predicates such as pruning, boolean expressions, execs, deletes, path regexes, and unknown tests pass through to the real tool for exact semantics.
- Complex search commands with filtering, sorting, context, or alternate pattern flags compact parsed raw output instead of approximating the command semantics.
- Compacted output must preserve exit code, stderr, file paths, line numbers, exact matched lines, errors, and truncation notices.
- Optimized raw probes stream stdout/stderr and may cap stored stdout for compaction; any cap is disclosed, stderr remains preserved, and `--raw` ignores the cap for exact output.
- Truncated proxy output should include a raw rerun hint and, when the raw output is large enough, a `.agentgrep/tee` full-output file.
- `--raw` and `AGENTGREP_DISABLE=1` bypass active agentgrep shim directories before running the underlying command.
- `AGENTGREP_DISABLE=1` disables proxy optimization for `agentgrep run`.
- `AGENTGREP_TEE=0` disables full-output tee files.
- Shims read output defaults from `AGENTGREP_LIMIT`, `AGENTGREP_BUDGET`, `AGENTGREP_RAW`, `AGENTGREP_JSON`, and `AGENTGREP_EXACT`.
- Trace recording stores command metadata only, never stdout/stderr content.
- Codex trace import reconstructs streamed function-call argument deltas before writing replayable command metadata.
- Claude trace import reads Bash tool-call command metadata from `~/.claude/projects` under the requested cwd subtree.
- Trace replay only executes safe read-only discovery commands; mutating git, unsupported commands, shell control operators, and redirections are skipped.
- Shims are opt-in, reversible, refuse to overwrite non-agentgrep files unless `--force` is passed, and resolve the real executable before proxying to avoid recursion.
- Shim status reports when the shim directory is present in `PATH` but shadowed by earlier system paths.
- Shims pass piped or file stdin directly to the real executable so Unix pipelines keep normal streaming behavior.
- Shims decline optimization when the parent shell command contains a pipeline or redirection, preserving stream semantics such as `find ... | head`.

RTK reference notes:

- Prefer passthrough over clever filtering when command intent is unclear.
- Let filters decline and fall back to raw output.
- Keep stderr visible.
- Tee raw output for failures or truncation so the agent has a recovery path.
