## 2026-04-29
- BREAKING: `timeoutMs` is now mandatory on the `execute` RPC. Calls without it (or with 0) return an error: `timeoutMs required (>0 ms)`. Children exceeding the budget are killed via `kill_tree` and the task is failed with `execution timed out after <N> ms`.
- BREAKING: `rs-exec exec` and `rs-exec bash` CLI subcommands require `--timeout <ms>` (must be >0). clap rejects missing/zero values.
- `run_code` plumbs the timeout through to the RPC and uses `timeout + 5000ms` as the rpc_client read deadline.
- New env override: `RS_EXEC_PORT_FILE` lets a runner+CLI use a private port file for isolated test runs.

## 2026-04-12
- managed_browser_user_data: use cwd/.plugkit-browser-profile instead of %LOCALAPPDATA%/plugkit/browser-profiles/<session_id>
- Auto-add .plugkit-browser-profile to .gitignore on first browser launch
- Remove browser kill from idle session cleanup — browser survives idle timeout, only tasks cleaned
