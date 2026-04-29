## 2026-04-29 (hotfix)
- `execute` RPC no longer rejects missing/zero `timeoutMs`. Instead it applies a 5-minute (300_000 ms) default and logs a stderr deprecation warning so existing callers (rs-plugkit, hook code) keep working while they migrate. The earlier hard rejection caused a process-spawn cascade in production when downstream callers had not yet been updated. Children exceeding the budget are still killed via `kill_tree` and the task still fails with `execution timed out after <N> ms` — the safety guarantee is preserved.
- CLI `rs-exec exec` and `rs-exec bash` continue to require `--timeout <ms>` (clap rejects missing/zero) — fresh interface, no legacy callers.
- `run_code` plumbs the timeout through to the RPC and uses `timeout + 5000ms` as the rpc_client read deadline.
- New env override: `RS_EXEC_PORT_FILE` lets a runner+CLI use a private port file for isolated test runs.

## 2026-04-12
- managed_browser_user_data: use cwd/.plugkit-browser-profile instead of %LOCALAPPDATA%/plugkit/browser-profiles/<session_id>
- Auto-add .plugkit-browser-profile to .gitignore on first browser launch
- Remove browser kill from idle session cleanup — browser survives idle timeout, only tasks cleaned
