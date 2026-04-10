use axum::{extract::State, routing::{get, post}, Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::HashMap, env, fs, path::PathBuf, process::{ChildStdin, Command, Stdio}, sync::{Arc, Mutex}, time::{Duration, SystemTime, UNIX_EPOCH}};
use tokio::net::TcpListener;
use crate::background_tasks::{BackgroundTaskStore, TaskResult, TaskStatus};
use crate::runtime::kill_session_browser;

const IDLE_TIMEOUT_SECS: u64 = 15 * 60;

fn session_activity_file() -> PathBuf {
    env::temp_dir().join("plugkit-session-activity.json")
}

fn touch_session_activity(session_id: &str) {
    if session_id.is_empty() { return; }
    let path = session_activity_file();
    let mut map: serde_json::Map<String, Value> = fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    map.insert(session_id.to_string(), json!(now));
    let _ = fs::write(&path, serde_json::to_string(&map).unwrap_or_default());
}

fn cleanup_idle_sessions(store: &Arc<BackgroundTaskStore>, active: &Arc<Mutex<HashMap<u64, (u32, Option<ChildStdin>)>>>) {
    let path = session_activity_file();
    let map: serde_json::Map<String, Value> = match fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
    {
        Some(m) => m,
        None => return,
    };
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let mut dead_sessions: Vec<String> = Vec::new();
    for (sid, last_val) in &map {
        if let Some(last) = last_val.as_u64() {
            if now.saturating_sub(last) > IDLE_TIMEOUT_SECS {
                dead_sessions.push(sid.clone());
            }
        }
    }
    for sid in &dead_sessions {
        eprintln!("[runner] Session {} idle >15min — cleaning up.", sid);
        let task_ids = store.session_task_ids(sid);
        let pids: Vec<u32> = {
            let mut a = active.lock().unwrap();
            task_ids.iter().filter_map(|id| a.remove(id).map(|(pid, stdin)| { drop(stdin); pid })).collect()
        };
        for pid in pids { kill_pid(pid); }
        store.delete_session_tasks(sid);
        kill_session_browser(sid);
    }
    if !dead_sessions.is_empty() {
        let mut updated: serde_json::Map<String, Value> = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        for sid in &dead_sessions { updated.remove(sid); }
        let _ = fs::write(&path, serde_json::to_string(&updated).unwrap_or_default());
    }
}

fn port_file() -> PathBuf {
    env::temp_dir().join("glootie-runner.port")
}

fn self_exe() -> String {
    env::current_exe().unwrap_or_default().to_string_lossy().to_string()
}

pub struct AppState {
    store: Arc<BackgroundTaskStore>,
    active: Arc<Mutex<HashMap<u64, (u32, Option<ChildStdin>)>>>,
}

#[derive(Deserialize)]
pub struct RpcRequest {
    method: String,
    params: Option<Value>,
    id: Option<Value>,
}


async fn health() -> Json<Value> {
    Json(json!({ "ok": true }))
}

async fn rpc_handler(State(state): State<Arc<AppState>>, Json(req): Json<RpcRequest>) -> Json<Value> {
    let params = req.params.unwrap_or(json!({}));
    match handle_rpc(&state, &req.method, &params).await {
        Ok(result) => Json(json!({ "id": req.id, "result": result })),
        Err(e) => Json(json!({ "id": req.id, "error": { "message": e.to_string() } })),
    }
}

async fn handle_rpc(state: &Arc<AppState>, method: &str, params: &Value) -> anyhow::Result<Value> {
    match method {
        "execute" => {
            let code = params["code"].as_str().unwrap_or("").to_string();
            let runtime = params["runtime"].as_str().unwrap_or("nodejs").to_string();
            let cwd = params["workingDirectory"].as_str().unwrap_or(".").to_string();
            let _timeout = params["timeout"].as_u64().unwrap_or(15000); // accepted but unused; client polls
            // backgroundTaskId is no longer pre-created by the client; the runner always
            // creates the task atomically here so the ID is only issued after spawn succeeds.
            let task_id = params["backgroundTaskId"].as_u64().unwrap_or_else(|| state.store.create_task());
            if let Some(sid) = params["sessionId"].as_str() {
                state.store.set_session_id(task_id, sid);
            }
            let sid = params["sessionId"].as_str().unwrap_or("").to_string();
            touch_session_activity(&sid);
            spawn_exec_process(state, task_id, &code, &runtime, &cwd, &sid).await?;
            // Return the task ID immediately — the client polls via getTask/getAndClearOutput.
            // Do NOT block here until the task completes; that held the TCP connection for up
            // to 15s and caused the client's 2s RPC timeout to fire before the ID was returned,
            // making the task ID unretrievable ("Task not found" on the next plugkit status call).
            Ok(json!({ "result": { "backgroundTaskId": task_id, "persisted": true } }))
        }
        "createTask" => {
            let id = state.store.create_task();
            if let Some(sid) = params["sessionId"].as_str() {
                state.store.set_session_id(id, sid);
            }
            Ok(json!({ "taskId": id }))
        }
        "deleteSessionTasks" => {
            let sid = params["sessionId"].as_str().unwrap_or("");
            if sid.is_empty() { return Ok(json!({ "deleted": 0 })); }
            touch_session_activity(sid);
            let task_ids = state.store.session_task_ids(sid);
            let pids: Vec<u32> = {
                let mut active = state.active.lock().unwrap();
                task_ids.iter().filter_map(|id| active.remove(id).map(|(pid, stdin)| { drop(stdin); pid })).collect()
            };
            for pid in pids { kill_pid(pid); }
            let count = state.store.delete_session_tasks(sid);
            kill_session_browser(sid);
            Ok(json!({ "deleted": count }))
        }
        "startTask" => {
            let id = params["taskId"].as_u64().unwrap_or(0);
            state.store.start_task(id);
            Ok(json!({}))
        }
        "completeTask" => {
            let id = params["taskId"].as_u64().unwrap_or(0);
            let r = &params["result"];
            state.store.complete_task(id, TaskResult {
                success: r["success"].as_bool().unwrap_or(false),
                stdout: r["stdout"].as_str().unwrap_or("").to_string(),
                stderr: r["stderr"].as_str().unwrap_or("").to_string(),
                error: r["error"].as_str().map(|s| s.to_string()),
                exit_code: r["exitCode"].as_i64().unwrap_or(0) as i32,
            });
            Ok(json!({}))
        }
        "failTask" => {
            let id = params["taskId"].as_u64().unwrap_or(0);
            let e = params["error"].as_str().unwrap_or("unknown error").to_string();
            state.store.fail_task(id, e);
            Ok(json!({}))
        }
        "getTask" => {
            let id = params["taskId"].as_u64().unwrap_or(0);
            match state.store.get_task_status(id) {
                None => Ok(json!({ "task": null })),
                Some((s, r)) => {
                    let status = match s { TaskStatus::Pending => "pending", TaskStatus::Running => "running", TaskStatus::Completed => "completed", TaskStatus::Failed => "failed" };
                    let result_val = r.map(|r| json!({ "success": r.success, "stdout": r.stdout, "stderr": r.stderr, "error": r.error, "exitCode": r.exit_code }));
                    Ok(json!({ "task": { "id": id, "status": status, "result": result_val } }))
                }
            }
        }
        "deleteTask" => {
            let id = params["taskId"].as_u64().unwrap_or(0);
            let session_id = state.store.get_task_session_id(id);
            let entry = state.active.lock().unwrap().remove(&id);
            let process_killed = if let Some((pid, stdin)) = entry {
                drop(stdin);
                kill_pid(pid);
                true
            } else {
                false
            };
            state.store.delete_task(id);
            let browser_killed = if let Some(ref sid) = session_id {
                if !sid.is_empty() && state.store.session_task_ids(sid).is_empty() {
                    kill_session_browser(sid);
                    true
                } else {
                    false
                }
            } else {
                false
            };
            Ok(json!({ "processKilled": process_killed, "browserSessionReleased": browser_killed }))
        }
        "listTasks" => {
            let tasks: Vec<Value> = state.store.list_tasks().iter().map(|(id, s)| {
                let status = match s { TaskStatus::Pending => "pending", TaskStatus::Running => "running", TaskStatus::Completed => "completed", TaskStatus::Failed => "failed" };
                json!({ "id": id, "status": status })
            }).collect();
            Ok(json!({ "tasks": tasks }))
        }
        "listSessionTasks" => {
            let sid = params["sessionId"].as_str().unwrap_or("");
            let ids = state.store.session_task_ids(sid);
            let tasks_lock = state.store.list_tasks();
            let tasks: Vec<Value> = tasks_lock.iter()
                .filter(|(id, _)| ids.contains(id))
                .map(|(id, s)| {
                    let status = match s { TaskStatus::Pending => "pending", TaskStatus::Running => "running", TaskStatus::Completed => "completed", TaskStatus::Failed => "failed" };
                    json!({ "id": id, "status": status })
                }).collect();
            Ok(json!({ "tasks": tasks }))
        }
        "drainSessionOutput" => {
            let sid = params["sessionId"].as_str().unwrap_or("");
            let entries = state.store.drain_session_output(sid);
            let tasks: Vec<Value> = entries.into_iter().map(|(id, status, output)| {
                let status_str = match status { TaskStatus::Pending => "pending", TaskStatus::Running => "running", TaskStatus::Completed => "completed", TaskStatus::Failed => "failed" };
                let out: Vec<Value> = output.into_iter().map(|e| json!({ "s": e.stream, "d": e.data })).collect();
                json!({ "id": id, "status": status_str, "output": out })
            }).collect();
            Ok(json!({ "tasks": tasks }))
        }
        "appendOutput" => {
            let id = params["taskId"].as_u64().unwrap_or(0);
            let t = params["type"].as_str().unwrap_or("stdout");
            let d = params["data"].as_str().unwrap_or("");
            state.store.append_output(id, t, d);
            Ok(json!({}))
        }
        "getAndClearOutput" => {
            let id = params["taskId"].as_u64().unwrap_or(0);
            let log = state.store.get_and_clear_output(id);
            let entries: Vec<Value> = log.iter().map(|e| json!({ "s": e.stream, "d": e.data })).collect();
            Ok(json!({ "output": entries }))
        }
        "waitForOutput" => {
            let id = params["taskId"].as_u64().unwrap_or(0);
            let timeout = params["timeoutMs"].as_u64().unwrap_or(30000);
            let timed_out = !state.store.wait_for_output(id, timeout).await;
            Ok(json!({ "timedOut": timed_out }))
        }
        "sendStdin" => {
            let id = params["taskId"].as_u64().unwrap_or(0);
            let data = params["data"].as_str().unwrap_or("").to_string();
            let mut active = state.active.lock().unwrap();
            let ok = if let Some((_, Some(stdin))) = active.get_mut(&id) {
                use std::io::Write;
                stdin.write_all(data.as_bytes()).is_ok()
            } else {
                false
            };
            Ok(json!({ "ok": ok }))
        }
        "pm2list" => {
            let active = state.active.lock().unwrap();
            let procs: Vec<Value> = active.iter().map(|(id, (pid, _))| json!({ "name": format!("rs-exec-task-{}", id), "status": "online", "pid": pid })).collect();
            Ok(json!({ "processes": procs }))
        }
        "killPort" => {
            let port = params["port"].as_u64().unwrap_or(0) as u16;
            if port == 0 { return Ok(json!({ "ok": false, "error": "port required" })); }
            let output = std::process::Command::new("netstat").args(["-ano"]).output()?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let port_str = format!(":{}", port);
            let mut killed_pid = 0u32;
            for line in stdout.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 5 && parts[1].ends_with(&port_str) && parts[3] == "LISTENING" {
                    if let Ok(pid) = parts[4].parse::<u32>() {
                        kill_pid(pid);
                        killed_pid = pid;
                        break;
                    }
                }
            }
            Ok(json!({ "ok": killed_pid != 0, "killedPid": killed_pid }))
        }
        "shutdown" => {
            tokio::spawn(async { tokio::time::sleep(Duration::from_millis(100)).await; std::process::exit(0); });
            Ok(json!({ "ok": true }))
        }
        _ => Err(anyhow::anyhow!("Unknown method: {}", method))
    }
}

async fn spawn_exec_process(state: &Arc<AppState>, task_id: u64, code: &str, runtime: &str, cwd: &str, session_id: &str) -> anyhow::Result<()> {
    let port = fs::read_to_string(port_file())?.trim().parse::<u16>()?;
    let code_file = env::temp_dir().join(format!("rs-exec-code-{}.txt", task_id));
    fs::write(&code_file, code)?;
    let mut env_vars = env::vars().collect::<HashMap<_,_>>();
    env_vars.remove("PORT");
    env_vars.insert("TASK_ID".into(), task_id.to_string());
    env_vars.insert("GM_EXEC_RPC_PORT".into(), port.to_string());
    env_vars.insert("RUNTIME".into(), runtime.to_string());
    env_vars.insert("CWD".into(), cwd.to_string());
    env_vars.insert("CODE_FILE".into(), code_file.to_string_lossy().to_string());
    if !session_id.is_empty() { env_vars.insert("SESSION_ID".into(), session_id.to_string()); }
    #[cfg(windows)]
    let mut child = {
        use std::os::windows::process::CommandExt;
        Command::new(self_exe())
            .arg("--exec-process-mode")
            .envs(&env_vars)
            .current_dir(cwd)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .creation_flags(0x08000000)
            .spawn()?
    };
    #[cfg(not(windows))]
    let mut child = Command::new(self_exe())
        .arg("--exec-process-mode")
        .envs(&env_vars)
        .current_dir(cwd)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn()?;
    let pid = child.id();
    let stdin = child.stdin.take();
    state.active.lock().unwrap().insert(task_id, (pid, stdin));
    state.store.start_task(task_id);
    std::mem::forget(child);
    Ok(())
}

fn kill_pid(pid: u32) {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, false);
    if let Some(proc) = sys.process(sysinfo::Pid::from_u32(pid)) {
        proc.kill();
    }
}

pub async fn run_server() -> anyhow::Result<()> {
    let store = BackgroundTaskStore::new();
    let state = Arc::new(AppState { store, active: Arc::new(Mutex::new(HashMap::new())) });
    let cleanup_store = state.store.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
        loop {
            interval.tick().await;
            cleanup_store.cleanup_old_tasks();
        }
    });
    let idle_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            cleanup_idle_sessions(&idle_state.store, &idle_state.active);
        }
    });
    let app = Router::new()
        .route("/health", get(health))
        .route("/rpc", post(rpc_handler))
        .with_state(state);
    const FIXED_PORT: u16 = 32882;
    // Retry binding fixed port for up to 2s in case a previous runner is still shutting down
    for attempt in 0..5 {
        match TcpListener::bind(format!("127.0.0.1:{}", FIXED_PORT)).await {
            Ok(listener) => {
                fs::write(port_file(), FIXED_PORT.to_string())?;
                eprintln!("[runner] listening on port {}", FIXED_PORT);
                axum::serve(listener, app).await?;
                return Ok(());
            }
            Err(_) => {
                if attempt < 4 {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
        }
    }
    Err(anyhow::anyhow!("Could not bind fixed port {} after retries", FIXED_PORT))
}
