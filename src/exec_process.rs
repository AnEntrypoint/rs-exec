use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::env;
use std::fs;
use std::time::Duration;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

fn spawn_no_window(cmd: &mut Command) -> std::io::Result<Child> {
    #[cfg(windows)]
    { cmd.creation_flags(CREATE_NO_WINDOW).spawn() }
    #[cfg(not(windows))]
    { cmd.spawn() }
}

mod runtime;

fn rpc_sync(port: u16, method: &str, params: serde_json::Value) {
    let body = serde_json::json!({ "method": method, "params": params }).to_string();
    let Ok(mut stream) = TcpStream::connect_timeout(
        &format!("127.0.0.1:{}", port).parse().unwrap(),
        Duration::from_millis(5000),
    ) else { return };
    let _ = stream.set_write_timeout(Some(Duration::from_millis(5000)));
    let req = format!(
        "POST /rpc HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        port, body.len(), body
    );
    let _ = stream.write_all(req.as_bytes());
    let mut buf = [0u8; 512];
    let _ = stream.read(&mut buf);
}

fn main() {
    let task_id: u64 = env::var("TASK_ID").unwrap_or_default().parse().unwrap_or(0);
    let port: u16 = env::var("GM_EXEC_RPC_PORT").or_else(|_| env::var("PORT")).unwrap_or_default().parse().unwrap_or(0);
    let runtime = env::var("RUNTIME").unwrap_or_else(|_| "nodejs".into());
    let cwd = env::var("CWD").unwrap_or_else(|_| env::current_dir().unwrap().to_string_lossy().into());
    let code_file = env::var("CODE_FILE").unwrap_or_default();

    let code = fs::read_to_string(&code_file).unwrap_or_default();
    let _ = fs::remove_file(&code_file);
    let session_id = env::var("SESSION_ID").unwrap_or_default();

    let spawn_result = match runtime::spawn_process(&runtime, &code, &cwd, &session_id) {
        Ok(r) => r,
        Err(e) => {
            rpc_sync(port, "completeTask", serde_json::json!({ "taskId": task_id, "result": { "success": false, "exitCode": 1, "stdout": "", "stderr": e.to_string(), "error": e.to_string() } }));
            return;
        }
    };

    if let Some(phase) = spawn_result.compile_phase {
        run_compiled(task_id, port, spawn_result.child, phase);
    } else {
        run_child_sync(task_id, port, spawn_result.child);
    }

}

fn run_child_sync(task_id: u64, port: u16, mut child: std::process::Child) {
    use std::sync::{Arc, Mutex};
    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();
    let out = Arc::new(Mutex::new(String::new()));
    let err = Arc::new(Mutex::new(String::new()));

    let out_t = out.clone();
    let stdout_thread = stdout_handle.map(|mut s| {
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match s.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                        out_t.lock().unwrap().push_str(&chunk);
                        rpc_sync(port, "appendOutput", serde_json::json!({ "taskId": task_id, "type": "stdout", "data": chunk }));
                    }
                }
            }
        })
    });

    let err_t = err.clone();
    let stderr_thread = stderr_handle.map(|mut s| {
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match s.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                        err_t.lock().unwrap().push_str(&chunk);
                        rpc_sync(port, "appendOutput", serde_json::json!({ "taskId": task_id, "type": "stderr", "data": chunk }));
                    }
                }
            }
        })
    });

    if let Some(t) = stdout_thread { let _ = t.join(); }
    if let Some(t) = stderr_thread { let _ = t.join(); }
    let code = child.wait().map(|s| s.code().unwrap_or(1)).unwrap_or(1);
    let out = out.lock().unwrap().clone();
    let err = err.lock().unwrap().clone();
    rpc_sync(port, "completeTask", serde_json::json!({ "taskId": task_id, "result": { "success": code == 0, "exitCode": code, "stdout": out, "stderr": err, "error": serde_json::Value::Null } }));
}

fn run_compiled(task_id: u64, port: u16, mut compile_child: std::process::Child, phase: runtime::CompilePhase) {
    let mut buf = [0u8; 4096];
    let mut compile_stdout = String::new();
    let mut compile_stderr = String::new();
    if let Some(mut s) = compile_child.stdout.take() {
        loop { match s.read(&mut buf) { Ok(0)|Err(_) => break, Ok(n) => compile_stdout.push_str(&String::from_utf8_lossy(&buf[..n])) } }
    }
    if let Some(mut s) = compile_child.stderr.take() {
        loop { match s.read(&mut buf) { Ok(0)|Err(_) => break, Ok(n) => compile_stderr.push_str(&String::from_utf8_lossy(&buf[..n])) } }
    }
    let compile_code = compile_child.wait().map(|s| s.code().unwrap_or(1)).unwrap_or(1);
    if compile_code != 0 {
        rpc_sync(port, "completeTask", serde_json::json!({ "taskId": task_id, "result": { "success": false, "exitCode": 1, "stdout": compile_stdout, "stderr": compile_stderr, "error": "Compilation failed" } }));
        return;
    }
    let run_child = if phase.runtime == "java" {
        let cp = phase.cp.as_deref().unwrap_or(&phase.cwd);
        let cn = phase.class_name.as_deref().unwrap_or("Main");
        spawn_no_window(Command::new("java").args(["-cp", cp, cn])
            .current_dir(&phase.cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()))
    } else {
        spawn_no_window(Command::new(&phase.bin_path)
            .current_dir(&phase.cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()))
    };
    match run_child {
        Err(e) => rpc_sync(port, "completeTask", serde_json::json!({ "taskId": task_id, "result": { "success": false, "exitCode": 1, "stdout": "", "stderr": e.to_string(), "error": e.to_string() } })),
        Ok(child) => run_child_sync(task_id, port, child),
    }
}
