use axum::{extract::State, Json};
use serde_json::{json, Value};
use std::{collections::HashMap, process::{Command, Stdio}, sync::Arc, env, fs, time::Duration};
use crate::background_tasks::{TaskResult, TaskStatus};
use crate::runtime::kill_session_browser;
use crate::runner::{AppState, touch_session_activity, port_file, self_exe};

pub async fn health() -> Json<Value> { Json(json!({ "ok": true })) }

pub async fn rpc_handler(State(state): State<Arc<AppState>>, Json(req): Json<crate::runner::RpcRequest>) -> Json<Value> {
    let params = req.params.unwrap_or(json!({}));
    match handle_rpc(&state, &req.method, &params).await {
        Ok(result) => Json(json!({ "id": req.id, "result": result })),
        Err(e) => Json(json!({ "id": req.id, "error": { "message": e.to_string() } })),
    }
}

pub async fn handle_rpc(state: &Arc<AppState>, method: &str, params: &Value) -> anyhow::Result<Value> {
    match method {
        "execute" => {
            let code = params["code"].as_str().unwrap_or("").to_string();
            let runtime = params["runtime"].as_str().unwrap_or("nodejs").to_string();
            let cwd = params["workingDirectory"].as_str().unwrap_or(".").to_string();
            let _timeout = params["timeout"].as_u64().unwrap_or(15000);
            let task_id = params["backgroundTaskId"].as_u64().unwrap_or_else(|| state.store.create_task());
            if let Some(sid) = params["sessionId"].as_str() { state.store.set_session_id(task_id, sid); }
            let sid = params["sessionId"].as_str().unwrap_or("").to_string();
            touch_session_activity(&sid);
            spawn_exec_process(state, task_id, &code, &runtime, &cwd, &sid).await?;
            Ok(json!({ "result": { "backgroundTaskId": task_id, "persisted": true } }))
        }
        "createTask" => {
            let id = state.store.create_task();
            if let Some(sid) = params["sessionId"].as_str() { state.store.set_session_id(id, sid); }
            Ok(json!({ "taskId": id }))
        }
        "deleteSessionTasks" => {
            let sid = params["sessionId"].as_str().unwrap_or("");
            if sid.is_empty() { return Ok(json!({ "deleted": 0 })); }
            touch_session_activity(sid);
            let task_ids = state.store.session_task_ids(sid);
            let pids: Vec<u32> = { let mut a = state.active.lock().unwrap(); task_ids.iter().filter_map(|id| a.remove(id).map(|(pid, stdin)| { drop(stdin); pid })).collect() };
            for pid in pids { crate::kill::kill_tree(pid); }
            let count = state.store.delete_session_tasks(sid);
            kill_session_browser(sid);
            Ok(json!({ "deleted": count }))
        }
        "startTask" => { state.store.start_task(params["taskId"].as_u64().unwrap_or(0)); Ok(json!({})) }
        "completeTask" => {
            let id = params["taskId"].as_u64().unwrap_or(0);
            let r = &params["result"];
            state.store.complete_task(id, TaskResult { success: r["success"].as_bool().unwrap_or(false), stdout: r["stdout"].as_str().unwrap_or("").to_string(), stderr: r["stderr"].as_str().unwrap_or("").to_string(), error: r["error"].as_str().map(|s| s.to_string()), exit_code: r["exitCode"].as_i64().unwrap_or(0) as i32 });
            Ok(json!({}))
        }
        "failTask" => { state.store.fail_task(params["taskId"].as_u64().unwrap_or(0), params["error"].as_str().unwrap_or("unknown error").to_string()); Ok(json!({})) }
        "getTask" => {
            let id = params["taskId"].as_u64().unwrap_or(0);
            let req_sid = params["sessionId"].as_str().unwrap_or("");
            if !req_sid.is_empty() { if let Some(task_sid) = state.store.get_task_session_id(id) { if task_sid != req_sid { return Ok(json!({ "task": null })); } } }
            match state.store.get_task_status(id) {
                None => Ok(json!({ "task": null })),
                Some((s, r)) => { let status = match s { TaskStatus::Pending => "pending", TaskStatus::Running => "running", TaskStatus::Completed => "completed", TaskStatus::Failed => "failed" }; let result_val = r.map(|r| json!({ "success": r.success, "stdout": r.stdout, "stderr": r.stderr, "error": r.error, "exitCode": r.exit_code })); Ok(json!({ "task": { "id": id, "status": status, "result": result_val } })) }
            }
        }
        "deleteTask" => {
            let id = params["taskId"].as_u64().unwrap_or(0);
            let session_id = state.store.get_task_session_id(id);
            let entry = state.active.lock().unwrap().remove(&id);
            let process_killed = if let Some((pid, stdin)) = entry { drop(stdin); crate::kill::kill_tree(pid); true } else { false };
            state.store.delete_task(id);
            let browser_killed = if let Some(ref sid) = session_id { if !sid.is_empty() && state.store.session_task_ids(sid).is_empty() { kill_session_browser(sid); true } else { false } } else { false };
            Ok(json!({ "processKilled": process_killed, "browserSessionReleased": browser_killed }))
        }
        "listTasks" => { let tasks: Vec<Value> = state.store.list_tasks().iter().map(|(id, s)| { let status = match s { TaskStatus::Pending => "pending", TaskStatus::Running => "running", TaskStatus::Completed => "completed", TaskStatus::Failed => "failed" }; json!({ "id": id, "status": status }) }).collect(); Ok(json!({ "tasks": tasks })) }
        "listSessionTasks" => {
            let sid = params["sessionId"].as_str().unwrap_or("");
            let ids = state.store.session_task_ids(sid);
            let tasks_lock = state.store.list_tasks();
            let tasks: Vec<Value> = tasks_lock.iter().filter(|(id, _)| ids.contains(id)).map(|(id, s)| { let status = match s { TaskStatus::Pending => "pending", TaskStatus::Running => "running", TaskStatus::Completed => "completed", TaskStatus::Failed => "failed" }; json!({ "id": id, "status": status }) }).collect();
            Ok(json!({ "tasks": tasks }))
        }
        "drainSessionOutput" => { let sid = params["sessionId"].as_str().unwrap_or(""); let entries = state.store.drain_session_output(sid); let tasks: Vec<Value> = entries.into_iter().map(|(id, status, output)| { let status_str = match status { TaskStatus::Pending => "pending", TaskStatus::Running => "running", TaskStatus::Completed => "completed", TaskStatus::Failed => "failed" }; let out: Vec<Value> = output.into_iter().map(|e| json!({ "s": e.stream, "d": e.data })).collect(); json!({ "id": id, "status": status_str, "output": out }) }).collect(); Ok(json!({ "tasks": tasks })) }
        "appendOutput" => { state.store.append_output(params["taskId"].as_u64().unwrap_or(0), params["type"].as_str().unwrap_or("stdout"), params["data"].as_str().unwrap_or("")); Ok(json!({})) }
        "getAndClearOutput" => {
            let id = params["taskId"].as_u64().unwrap_or(0);
            let req_sid = params["sessionId"].as_str().unwrap_or("");
            if !req_sid.is_empty() { if let Some(task_sid) = state.store.get_task_session_id(id) { if task_sid != req_sid { return Ok(json!({ "output": [] })); } } }
            let entries: Vec<Value> = state.store.get_and_clear_output(id).iter().map(|e| json!({ "s": e.stream, "d": e.data })).collect();
            Ok(json!({ "output": entries }))
        }
        "waitForOutput" => {
            let id = params["taskId"].as_u64().unwrap_or(0);
            let req_sid = params["sessionId"].as_str().unwrap_or("");
            if !req_sid.is_empty() { if let Some(task_sid) = state.store.get_task_session_id(id) { if task_sid != req_sid { return Ok(json!({ "timedOut": true })); } } }
            let timed_out = !state.store.wait_for_output(id, params["timeoutMs"].as_u64().unwrap_or(30000)).await;
            Ok(json!({ "timedOut": timed_out }))
        }
        "sendStdin" => {
            let id = params["taskId"].as_u64().unwrap_or(0);
            let data = params["data"].as_str().unwrap_or("").to_string();
            let mut active = state.active.lock().unwrap();
            let ok = if let Some((_, Some(stdin))) = active.get_mut(&id) { use std::io::Write; stdin.write_all(data.as_bytes()).is_ok() } else { false };
            Ok(json!({ "ok": ok }))
        }
        "pm2list" => { let active = state.active.lock().unwrap(); let procs: Vec<Value> = active.iter().map(|(id, (pid, _))| json!({ "name": format!("rs-exec-task-{}", id), "status": "online", "pid": pid })).collect(); Ok(json!({ "processes": procs })) }
        "killPort" => {
            let port = params["port"].as_u64().unwrap_or(0) as u16;
            if port == 0 { return Ok(json!({ "ok": false, "error": "port required" })); }
            let output = std::process::Command::new("netstat").args(["-ano"]).output()?;
            let port_str = format!(":{}", port);
            let mut killed_pid = 0u32;
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 5 && parts[1].ends_with(&port_str) && parts[3] == "LISTENING" { if let Ok(pid) = parts[4].parse::<u32>() { crate::kill::kill_tree(pid); killed_pid = pid; break; } }
            }
            Ok(json!({ "ok": killed_pid != 0, "killedPid": killed_pid }))
        }
        "shutdown" => { tokio::spawn(async { tokio::time::sleep(Duration::from_millis(100)).await; std::process::exit(0); }); Ok(json!({ "ok": true })) }
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
    let mut child = { use std::os::windows::process::CommandExt; Command::new(self_exe()).arg("--exec-process-mode").envs(&env_vars).current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).creation_flags(0x08000000).spawn()? };
    #[cfg(not(windows))]
    let mut child = Command::new(self_exe()).arg("--exec-process-mode").envs(&env_vars).current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let pid = child.id();
    let stdin = child.stdin.take();
    state.active.lock().unwrap().insert(task_id, (pid, stdin));
    state.store.start_task(task_id);
    drop(child);
    Ok(())
}
