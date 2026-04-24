use axum::{routing::{get, post}, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::HashMap, env, fs, path::PathBuf, process::ChildStdin, sync::{Arc, Mutex}, time::{SystemTime, UNIX_EPOCH}};
use tokio::net::TcpListener;
use crate::background_tasks::BackgroundTaskStore;
use sysinfo::{ProcessesToUpdate, System};

const IDLE_TIMEOUT_SECS: u64 = 15 * 60;

pub fn session_activity_file() -> PathBuf { env::temp_dir().join("plugkit-session-activity.json") }

pub fn touch_session_activity(session_id: &str) {
    if session_id.is_empty() { return; }
    let path = session_activity_file();
    let mut map: serde_json::Map<String, Value> = fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    map.insert(session_id.to_string(), json!(now));
    let _ = fs::write(&path, serde_json::to_string(&map).unwrap_or_default());
}

fn cleanup_idle_sessions(store: &Arc<BackgroundTaskStore>, active: &Arc<Mutex<HashMap<u64, (u32, Option<ChildStdin>)>>>) {
    let path = session_activity_file();
    let map: serde_json::Map<String, Value> = match fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()) { Some(m) => m, None => return };
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let dead: Vec<String> = map.iter().filter_map(|(sid, v)| v.as_u64().filter(|&t| now.saturating_sub(t) > IDLE_TIMEOUT_SECS).map(|_| sid.clone())).collect();
    for sid in &dead {
        eprintln!("[runner] session {} idle >15min, cleaning up", sid);
        let task_ids = store.session_task_ids(sid);
        let pids: Vec<u32> = { let mut a = active.lock().unwrap(); task_ids.iter().filter_map(|id| a.remove(id).map(|(pid, stdin)| { drop(stdin); pid })).collect() };
        for pid in pids { crate::kill::kill_tree(pid); }
        store.delete_session_tasks(sid);
    }
    if !dead.is_empty() {
        let mut updated: serde_json::Map<String, Value> = fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        for sid in &dead { updated.remove(sid); }
        let _ = fs::write(&path, serde_json::to_string(&updated).unwrap_or_default());
    }
}

pub fn port_file() -> PathBuf { env::temp_dir().join("glootie-runner.port") }
pub fn self_exe() -> String { env::current_exe().unwrap_or_default().to_string_lossy().to_string() }

pub struct AppState {
    pub store: Arc<BackgroundTaskStore>,
    pub active: Arc<Mutex<HashMap<u64, (u32, Option<ChildStdin>)>>>,
}

#[derive(Deserialize)]
pub struct RpcRequest {
    pub method: String,
    pub params: Option<Value>,
    pub id: Option<Value>,
}

fn reap_orphaned_exec_processes() {
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, false);
    let runner_pids: std::collections::HashSet<u32> = sys.processes().values()
        .filter(|p| {
            let cmd = p.cmd().iter().map(|s| s.to_string_lossy()).collect::<Vec<_>>().join(" ");
            cmd.contains("--runner-mode")
        })
        .map(|p| p.pid().as_u32())
        .collect();
    let now_secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let orphan_pids: Vec<u32> = sys.processes().values()
        .filter(|p| {
            let cmd = p.cmd().iter().map(|s| s.to_string_lossy()).collect::<Vec<_>>().join(" ");
            if !cmd.contains("--exec-process-mode") { return false; }
            let age = now_secs.saturating_sub(p.start_time());
            if age < 5 { return false; }
            let parent = p.parent().map(|pp| pp.as_u32()).unwrap_or(0);
            !runner_pids.contains(&parent)
        })
        .map(|p| p.pid().as_u32())
        .collect();
    let count = orphan_pids.len();
    for pid in orphan_pids { crate::kill::kill_tree(pid); }
    if count > 0 { eprintln!("[runner] reaped {} orphaned exec-process-mode processes", count); }
}

fn reap_orphaned_browsers() {
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, false);
    let runner_pids: std::collections::HashSet<u32> = sys.processes().values()
        .filter(|p| {
            let cmd = p.cmd().iter().map(|s| s.to_string_lossy()).collect::<Vec<_>>().join(" ");
            cmd.contains("--runner-mode")
        })
        .map(|p| p.pid().as_u32())
        .collect();
    let now_secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let orphan_roots: Vec<u32> = sys.processes().values()
        .filter(|p| {
            let cmd = p.cmd().iter().map(|s| s.to_string_lossy()).collect::<Vec<_>>().join(" ");
            if !cmd.contains(".plugkit-browser-profile") { return false; }
            let age = now_secs.saturating_sub(p.start_time());
            if age < 5 { return false; }
            let parent = p.parent().map(|pp| pp.as_u32()).unwrap_or(0);
            if runner_pids.contains(&parent) { return false; }
            let mut pp = p.parent();
            let mut hops = 0;
            while let Some(ppid) = pp {
                if hops > 8 { break; }
                if runner_pids.contains(&ppid.as_u32()) { return false; }
                if let Some(parent_proc) = sys.process(ppid) {
                    let pcmd = parent_proc.cmd().iter().map(|s| s.to_string_lossy()).collect::<Vec<_>>().join(" ");
                    if pcmd.contains(".plugkit-browser-profile") {
                        return false;
                    }
                    pp = parent_proc.parent();
                } else { break; }
                hops += 1;
            }
            true
        })
        .map(|p| p.pid().as_u32())
        .collect();
    let count = orphan_roots.len();
    for pid in orphan_roots { crate::kill::kill_tree(pid); }
    if count > 0 { eprintln!("[runner] reaped {} orphaned browser process trees", count); }
    reap_playwriter_ws_server(&runner_pids);
}

fn reap_playwriter_ws_server(runner_pids: &std::collections::HashSet<u32>) {
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, false);
    let now_secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let targets: Vec<u32> = sys.processes().values()
        .filter(|p| {
            let name = p.name().to_string_lossy().to_lowercase();
            let cmd = p.cmd().iter().map(|s| s.to_string_lossy()).collect::<Vec<_>>().join(" ");
            let is_ws = name.contains("playwriter-ws-server") || cmd.contains("start-relay-server.js") || cmd.contains("start-relay-server.ts");
            if !is_ws { return false; }
            let age = now_secs.saturating_sub(p.start_time());
            if age < 5 { return false; }
            let mut pp = p.parent();
            let mut hops = 0;
            while let Some(ppid) = pp {
                if hops > 8 { break; }
                if runner_pids.contains(&ppid.as_u32()) { return false; }
                pp = sys.process(ppid).and_then(|pr| pr.parent());
                hops += 1;
            }
            true
        })
        .map(|p| p.pid().as_u32())
        .collect();
    let count = targets.len();
    for pid in targets { crate::kill::kill_tree(pid); }
    if count > 0 { eprintln!("[runner] reaped {} orphaned playwriter-ws-server processes (will auto-restart on next use)", count); }
}

pub async fn run_server() -> anyhow::Result<()> {
    reap_orphaned_exec_processes();
    reap_orphaned_browsers();
    let store = BackgroundTaskStore::new();
    let state = Arc::new(AppState { store, active: Arc::new(Mutex::new(HashMap::new())) });
    let cleanup_store = state.store.clone();
    let cleanup_active = state.active.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
        loop { interval.tick().await; cleanup_store.cleanup_old_tasks(&cleanup_active); }
    });
    let idle_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop { interval.tick().await; cleanup_idle_sessions(&idle_state.store, &idle_state.active); }
    });
    let app = Router::new().route("/health", get(crate::rpc::health)).route("/rpc", post(crate::rpc::rpc_handler)).with_state(state);
    // Always bind to an OS-assigned port. A fixed "preferred port" creates
    // recurring orphaned-listener bugs on Windows when the previous runner
    // crashes ungracefully — Windows leaves the socket bound to a dead PID
    // that never releases until reboot, blocking every subsequent runner.
    // The port file is the single source of truth; it is written atomically
    // before serving begins so clients always see the live port or a missing
    // file (which they treat as "no runner").
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let pf = port_file();
    let tmp = pf.with_extension("port.tmp");
    fs::write(&tmp, port.to_string())?;
    fs::rename(&tmp, &pf)?;
    eprintln!("[DAEMON:fsm] Listening {{ port: {}, pid: {} }}", port, std::process::id());
    let serve_result = axum::serve(listener, app).await;
    if let Err(e) = &serve_result {
        let crash_path = env::temp_dir().join("rs-exec-daemon-crash.log");
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
        let _ = fs::write(&crash_path, format!("ts={}\npid={}\nreason={}\n", ts, std::process::id(), e));
        eprintln!("[DAEMON:fsm] Crashed {{ reason: {}, written_to: {} }}", e, crash_path.display());
    }
    serve_result?;
    Ok(())
}
