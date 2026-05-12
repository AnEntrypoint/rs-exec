## 2026-05-12 ci: cascade fires on pull_request (build smoke, no release)

- `.github/workflows/cascade.yml`: added `pull_request: [main]` trigger. PRs now dispatch `rs-plugkit/build.yml` (downstream build smoke) so the cross-repo cascade is exercised before merge; pushes to main still dispatch `release.yml` as before. Closes the gap where cascade only verified after merge — symptoms previously masked until publish-binaries failed on `main`.

## 2026-05-12 spool: fix plugkit discovery + code-file race

- `which_plugkit`: add `PLUGKIT_BIN` env, `current_exe` fallback (the watcher process is plugkit itself when rs-exec is linked in), and a last-resort current_exe. Previously codesearch/recall/memorize/search verbs failed with "plugkit not found in PATH" whenever `CLAUDE_PLUGIN_ROOT/bin/plugkit` was absent (e.g. versioned cache layouts like `/root/.cache/plugkit/bin/v0.1.346/plugkit-linux-x64`).
- `run_request_raw`: write `<task_id>.code` to `spool_root()/work/` instead of `pending_dir()`. The watcher scans `in/` and deletes unknown-extension files; the `.code` payload was being deleted out from under the plugkit child before it could `fs::read_to_string`, producing `Error: No such file or directory (os error 2)` for every bash/nodejs/python builtin dispatch.

## 2026-04-29 (hotfix)
- `execute` RPC no longer rejects missing/zero `timeoutMs`. Instead it applies a 5-minute (300_000 ms) default and logs a stderr deprecation warning so existing callers (rs-plugkit, hook code) keep working while they migrate. The earlier hard rejection caused a process-spawn cascade in production when downstream callers had not yet been updated. Children exceeding the budget are still killed via `kill_tree` and the task still fails with `execution timed out after <N> ms` — the safety guarantee is preserved.
- CLI `rs-exec exec` and `rs-exec bash` continue to require `--timeout <ms>` (clap rejects missing/zero) — fresh interface, no legacy callers.
- `run_code` plumbs the timeout through to the RPC and uses `timeout + 5000ms` as the rpc_client read deadline.
- New env override: `RS_EXEC_PORT_FILE` lets a runner+CLI use a private port file for isolated test runs.

## 2026-04-12
- managed_browser_user_data: use cwd/.plugkit-browser-profile instead of %LOCALAPPDATA%/plugkit/browser-profiles/<session_id>
- Auto-add .plugkit-browser-profile to .gitignore on first browser launch
- Remove browser kill from idle session cleanup — browser survives idle timeout, only tasks cleaned
