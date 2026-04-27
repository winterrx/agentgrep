# Agentgrep Proxy Safety

Agentgrep should never make an agent miss the thing it explicitly asked a shell command to show.

Safety policy:

- Unsupported commands pass through unchanged.
- Mutating git commands pass through unchanged.
- Small command output that fits `--budget` passes through unchanged.
- Explicit bounded reads, such as `head`, `tail`, `sed -n`, and small `cat`, preserve raw output when it fits the budget.
- Complex search commands with filtering, sorting, context, or alternate pattern flags compact parsed raw output instead of approximating the command semantics.
- Compacted output must preserve exit code, stderr, file paths, line numbers, exact matched lines, errors, and truncation notices.
- Truncated proxy output should include a raw rerun hint and, when the raw output is large enough, a `.agentgrep/tee` full-output file.
- `AGENTGREP_DISABLE=1` disables proxy optimization for `agentgrep run`.
- `AGENTGREP_TEE=0` disables full-output tee files.
- Trace recording stores command metadata only, never stdout/stderr content.
- Trace replay only executes safe read-only discovery commands; mutating git, unsupported commands, shell control operators, and redirections are skipped.
- Shims are opt-in, reversible, refuse to overwrite non-agentgrep files unless `--force` is passed, and resolve the real executable before proxying to avoid recursion.

RTK reference notes:

- Prefer passthrough over clever filtering when command intent is unclear.
- Let filters decline and fall back to raw output.
- Keep stderr visible.
- Tee raw output for failures or truncation so the agent has a recovery path.
