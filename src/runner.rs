use axum::{extract::State, routing::{get, post}, Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::HashMap, env, fs, path::PathBuf, process::{ChildStdin, Command, Stdio}, sync::{Arc, Mutex}, time::Duration};
use tokio::net::TcpListener;
use rand::Rng;
use crate::background_tasks::{BackgroundTaskStore, TaskResult, TaskStatus};

fn port_file() -> PathBuf {
    env::temp_dir().join("glootie-runner.port")
}

fn self_exe() -> String {
    env::current_exe().unwrap_or_default().to_string_lossy().to_string()
}

pub struct AppState {
    store: Arc<BackgroundTaskStore>,
    active: Mutex<HashMap<u64, (u32, Option<ChildStdin>)>>,
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
            let timeout = params["timeout"].as_u64().unwrap_or(15000);
            let task_id = params["backgroundTaskId"].as_u64().unwrap_or_else(|| state.store.create_task());
            if let Some(sid) = params["sessionId"].as_str() {
                state.store.set_session_id(task_id, sid);
            }
            spawn_exec_process(state, task_id, &code, &runtime, &cwd).await?;
            let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout);
            loop {
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                let _done = state.store.wait_for_output(task_id, 200).await;
                let status = state.store.get_task_status(task_id);
                if let Some((s, _)) = &status {
                    if *s == TaskStatus::Completed || *s == TaskStatus::Failed { break; }
                }
            }
            if let Some((s, result)) = state.store.get_task_status(task_id) {
                if s == TaskStatus::Completed || s == TaskStatus::Failed {
                    let entry = state.active.lock().unwrap().remove(&task_id);
                    drop(entry);
                    state.store.delete_task(task_id);
                    let r = result.unwrap_or(TaskResult { success: false, stdout: String::new(), stderr: String::new(), error: Some("no result".into()), exit_code: 1 });
                    return Ok(json!({ "result": { "success": r.success, "stdout": r.stdout, "stderr": r.stderr, "error": r.error, "exitCode": r.exit_code, "backgroundTaskId": task_id, "completed": true } }));
                }
            }
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
            let count = state.store.delete_session_tasks(sid);
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
            let entry = state.active.lock().unwrap().remove(&id);
            if let Some((pid, stdin)) = entry {
                drop(stdin);
                kill_pid(pid);
            }
            state.store.delete_task(id);
            Ok(json!({}))
        }
        "listTasks" => {
            let tasks: Vec<Value> = state.store.list_tasks().iter().map(|(id, s)| {
                let status = match s { TaskStatus::Pending => "pending", TaskStatus::Running => "running", TaskStatus::Completed => "completed", TaskStatus::Failed => "failed" };
                json!({ "id": id, "status": status })
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
        "shutdown" => {
            tokio::spawn(async { tokio::time::sleep(Duration::from_millis(100)).await; std::process::exit(0); });
            Ok(json!({ "ok": true }))
        }
        _ => Err(anyhow::anyhow!("Unknown method: {}", method))
    }
}

async fn spawn_exec_process(state: &Arc<AppState>, task_id: u64, code: &str, runtime: &str, cwd: &str) -> anyhow::Result<()> {
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
    let state = Arc::new(AppState { store, active: Mutex::new(HashMap::new()) });
    let app = Router::new()
        .route("/health", get(health))
        .route("/rpc", post(rpc_handler))
        .with_state(state);
    for _ in 0..10 {
        let port = rand::thread_rng().gen_range(30000u16..40000u16);
        match TcpListener::bind(format!("127.0.0.1:{}", port)).await {
            Ok(listener) => {
                fs::write(port_file(), port.to_string())?;
                eprintln!("[runner] listening on port {}", port);
                axum::serve(listener, app).await?;
                return Ok(());
            }
            Err(_) => continue,
        }
    }
    Err(anyhow::anyhow!("Could not bind port after 10 attempts"))
}
