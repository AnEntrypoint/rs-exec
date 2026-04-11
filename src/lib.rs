pub mod background_tasks;
pub mod kill;
pub mod daemon;
pub mod rpc;
pub mod rpc_client;
pub mod runner;
pub mod runtime;

pub fn run_exec_process() {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    let task_id: u64 = std::env::var("TASK_ID").unwrap_or_default().parse().unwrap_or(0);
    let port: u16 = std::env::var("GM_EXEC_RPC_PORT")
        .or_else(|_| std::env::var("PORT"))
        .unwrap_or_default()
        .parse()
        .unwrap_or(0);
    let rt = std::env::var("RUNTIME").unwrap_or_else(|_| "nodejs".into());
    let cwd = std::env::var("CWD")
        .unwrap_or_else(|_| std::env::current_dir().unwrap().to_string_lossy().into());
    let code_file = std::env::var("CODE_FILE").unwrap_or_default();
    let code = std::fs::read_to_string(&code_file).unwrap_or_default();
    let _ = std::fs::remove_file(&code_file);
    let session_id = std::env::var("SESSION_ID").unwrap_or_default();

    eprintln!("[exec-process] task={} runtime={} starting", task_id, rt);

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

    fn run_child(task_id: u64, port: u16, mut child: std::process::Child) {
        let mut out = String::new();
        let mut err = String::new();
        let mut buf = [0u8; 4096];
        if let Some(ref mut s) = child.stdout.take() {
            loop { match s.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                    out.push_str(&chunk);
                    rpc_sync(port, "appendOutput", serde_json::json!({ "taskId": task_id, "type": "stdout", "data": chunk }));
                }
            }}
        }
        if let Some(ref mut s) = child.stderr.take() {
            loop { match s.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                    err.push_str(&chunk);
                    rpc_sync(port, "appendOutput", serde_json::json!({ "taskId": task_id, "type": "stderr", "data": chunk }));
                }
            }}
        }
        let code = child.wait().map(|s| s.code().unwrap_or(1)).unwrap_or(1);
        eprintln!("[exec-process] task={} child exited code={}", task_id, code);
        rpc_sync(port, "completeTask", serde_json::json!({ "taskId": task_id, "result": { "success": code == 0, "exitCode": code, "stdout": out, "stderr": err, "error": serde_json::Value::Null } }));
    }

    let spawn_result = match runtime::spawn_process(&rt, &code, &cwd, &session_id) {
        Ok(r) => r,
        Err(e) => {
            rpc_sync(port, "completeTask", serde_json::json!({ "taskId": task_id, "result": { "success": false, "exitCode": 1, "stdout": "", "stderr": e.to_string(), "error": e.to_string() } }));
            return;
        }
    };

    if let Some(phase) = spawn_result.compile_phase {
        use std::process::{Command, Stdio};
        let mut buf = [0u8; 4096];
        let mut compile_stdout = String::new();
        let mut compile_stderr = String::new();
        let mut cc = spawn_result.child;
        if let Some(mut s) = cc.stdout.take() { loop { match s.read(&mut buf) { Ok(0)|Err(_) => break, Ok(n) => compile_stdout.push_str(&String::from_utf8_lossy(&buf[..n])) } } }
        if let Some(mut s) = cc.stderr.take() { loop { match s.read(&mut buf) { Ok(0)|Err(_) => break, Ok(n) => compile_stderr.push_str(&String::from_utf8_lossy(&buf[..n])) } } }
        let compile_code = cc.wait().map(|s| s.code().unwrap_or(1)).unwrap_or(1);
        if compile_code != 0 {
            rpc_sync(port, "completeTask", serde_json::json!({ "taskId": task_id, "result": { "success": false, "exitCode": 1, "stdout": compile_stdout, "stderr": compile_stderr, "error": "Compilation failed" } }));
            return;
        }
        #[cfg(windows)]
        let mk = |cmd: &mut Command| -> std::io::Result<std::process::Child> {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000).spawn()
        };
        #[cfg(not(windows))]
        let mk = |cmd: &mut Command| cmd.spawn();
        let run_child_res = if phase.runtime == "java" {
            let cp = phase.cp.as_deref().unwrap_or(&phase.cwd);
            let cn = phase.class_name.as_deref().unwrap_or("Main");
            mk(Command::new("java").args(["-cp", cp, cn])
                .current_dir(&phase.cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()))
        } else {
            mk(Command::new(&phase.bin_path)
                .current_dir(&phase.cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()))
        };
        match run_child_res {
            Err(e) => rpc_sync(port, "completeTask", serde_json::json!({ "taskId": task_id, "result": { "success": false, "exitCode": 1, "stdout": "", "stderr": e.to_string(), "error": e.to_string() } })),
            Ok(child) => run_child(task_id, port, child),
        }
    } else {
        run_child(task_id, port, spawn_result.child);
    }

    eprintln!("[exec-process] task={} done", task_id);
}
