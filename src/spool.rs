use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

fn spool_root() -> PathBuf {
    if let Ok(p) = std::env::var("RS_EXEC_SPOOL_DIR") {
        return PathBuf::from(p);
    }
    std::env::temp_dir().join("rs-exec-spool")
}

fn pending_dir() -> PathBuf {
    spool_root().join("in")
}

fn done_dir() -> PathBuf {
    spool_root().join("out")
}

fn run_request(path: &Path) {
    let raw = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return,
    };
    let _ = fs::remove_file(path);
    let req: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            let out_path = done_dir().join(path.file_name().unwrap_or_default()).with_extension("json");
            let _ = fs::create_dir_all(done_dir());
            let _ = fs::write(out_path, serde_json::json!({ "ok": false, "error": e.to_string() }).to_string());
            return;
        }
    };

    let code = req["code"].as_str().unwrap_or("");
    let lang = req["lang"].as_str().unwrap_or("nodejs");
    let cwd = req["cwd"].as_str().unwrap_or(".");
    let timeout_ms = req["timeoutMs"].as_u64().unwrap_or(300000);
    let session_id = req["sessionId"].as_str().unwrap_or("");
    let task_id = req["taskId"].as_u64().unwrap_or(0);

    let out_path = done_dir().join(format!("{}.json", task_id));
    let _ = fs::create_dir_all(done_dir());
    let code_path = pending_dir().join(format!("{}.code", task_id));
    let _ = fs::write(&code_path, code);

    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("rs-exec"));
    let child = Command::new(exe)
        .arg("exec")
        .arg("--lang").arg(lang)
        .arg("--cwd").arg(cwd)
        .arg("--timeout").arg(timeout_ms.to_string())
        .arg("--session").arg(session_id)
        .arg("--file").arg(&code_path)
        .spawn();

    if child.is_err() {
        let _ = fs::write(out_path, serde_json::json!({ "ok": false, "error": "failed to spawn exec" }).to_string());
        let _ = fs::remove_file(code_path);
        return;
    }
    let _ = fs::write(out_path, serde_json::json!({ "ok": true, "status": "started" }).to_string());
}

pub fn watch_once() {
    let _ = fs::create_dir_all(pending_dir());
    let _ = fs::create_dir_all(done_dir());
    if let Ok(rd) = fs::read_dir(pending_dir()) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) == Some("json") {
                run_request(&p);
            }
        }
    }
}

pub fn run_daemon() {
    loop {
        watch_once();
        std::thread::sleep(Duration::from_millis(300));
    }
}
