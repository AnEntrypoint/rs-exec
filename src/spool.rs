use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const BUILTIN_LANGS: &[&str] = &[
    "nodejs", "javascript", "node", "js",
    "python", "py",
    "bash", "sh", "shell", "zsh",
    "powershell", "ps1",
    "typescript", "ts",
    "go", "rust", "c", "cpp", "java", "deno",
    "cmd", "browser",
];

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

fn find_lang_plugin(lang: &str, cwd: &str) -> Option<PathBuf> {
    let filename = format!("{}.js", lang);
    let project_plugin = PathBuf::from(cwd).join("lang").join(&filename);
    if project_plugin.exists() {
        return Some(project_plugin);
    }
    if let Ok(plugin_root) = std::env::var("CLAUDE_PLUGIN_ROOT") {
        let global_plugin = PathBuf::from(plugin_root).join("lang").join(&filename);
        if global_plugin.exists() {
            return Some(global_plugin);
        }
    }
    None
}

fn is_builtin(lang: &str) -> bool {
    BUILTIN_LANGS.contains(&lang)
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

    let code = match req["code"].as_str() {
        Some(c) => c.to_string(),
        None => {
            let task_id = req["taskId"].as_u64().unwrap_or(0);
            let out_path = done_dir().join(format!("{}.json", task_id));
            let _ = fs::create_dir_all(done_dir());
            let _ = fs::write(out_path, serde_json::json!({ "ok": false, "error": "missing field: code" }).to_string());
            return;
        }
    };
    let lang = req["lang"].as_str().unwrap_or("nodejs").to_string();
    let cwd = req["cwd"].as_str().unwrap_or(".").to_string();
    let timeout_ms = req["timeoutMs"].as_u64().unwrap_or(300_000);
    let task_id = req["taskId"].as_u64().unwrap_or(0);

    let out_path = done_dir().join(format!("{}.json", task_id));
    let _ = fs::create_dir_all(done_dir());
    let code_path = pending_dir().join(format!("{}.code", task_id));
    let _ = fs::write(&code_path, &code);

    let out_path_clone = out_path.clone();
    let code_path_clone = code_path.clone();
    let lang_clone = lang.clone();
    let cwd_clone = cwd.clone();

    std::thread::spawn(move || {
        let result = execute_task(&code, &lang_clone, &cwd_clone, &code_path_clone, timeout_ms, task_id);
        let _ = fs::remove_file(&code_path_clone);
        let _ = fs::write(&out_path_clone, result.to_string());
    });
}

fn execute_task(
    code: &str,
    lang: &str,
    cwd: &str,
    code_path: &Path,
    timeout_ms: u64,
    task_id: u64,
) -> serde_json::Value {
    if is_builtin(lang) {
        run_builtin(code_path, lang, cwd, timeout_ms, task_id)
    } else {
        match find_lang_plugin(lang, cwd) {
            Some(plugin_path) => run_lang_plugin(&plugin_path, code, cwd, timeout_ms, task_id),
            None => serde_json::json!({
                "ok": false,
                "error": format!("unknown lang '{}': not a builtin and no lang plugin found", lang),
                "lang": lang,
                "taskId": task_id,
            }),
        }
    }
}

fn run_builtin(
    code_path: &Path,
    lang: &str,
    cwd: &str,
    timeout_ms: u64,
    task_id: u64,
) -> serde_json::Value {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("rs-exec"));
    let mut cmd = Command::new(&exe);
    cmd.arg("exec")
        .arg("--lang").arg(lang)
        .arg("--cwd").arg(cwd)
        .arg("--timeout").arg(timeout_ms.to_string())
        .arg("--file").arg(code_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return serde_json::json!({
            "ok": false,
            "error": format!("spawn failed: {}", e),
            "lang": lang,
            "taskId": task_id,
        }),
    };

    let deadline = Duration::from_millis(timeout_ms.saturating_add(5_000));
    let start = std::time::Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let exit_code = status.code().unwrap_or(1);
                let mut stdout_buf = Vec::new();
                let mut stderr_buf = Vec::new();
                if let Some(mut s) = child.stdout.take() { let _ = s.read_to_end(&mut stdout_buf); }
                if let Some(mut s) = child.stderr.take() { let _ = s.read_to_end(&mut stderr_buf); }
                let combined = format!(
                    "{}{}",
                    String::from_utf8_lossy(&stdout_buf),
                    String::from_utf8_lossy(&stderr_buf)
                );
                return serde_json::json!({
                    "ok": exit_code == 0,
                    "output": combined,
                    "exitCode": exit_code,
                    "lang": lang,
                    "taskId": task_id,
                });
            }
            Ok(None) => {
                if start.elapsed() >= deadline {
                    let _ = child.kill();
                    let mut stdout_buf = Vec::new();
                    let mut stderr_buf = Vec::new();
                    if let Some(mut s) = child.stdout.take() { let _ = s.read_to_end(&mut stdout_buf); }
                    if let Some(mut s) = child.stderr.take() { let _ = s.read_to_end(&mut stderr_buf); }
                    let partial = format!(
                        "{}{}",
                        String::from_utf8_lossy(&stdout_buf),
                        String::from_utf8_lossy(&stderr_buf)
                    );
                    return serde_json::json!({
                        "ok": false,
                        "timedOut": true,
                        "output": format!("partial...\n{}", partial),
                        "lang": lang,
                        "taskId": task_id,
                    });
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return serde_json::json!({
                "ok": false,
                "error": format!("wait error: {}", e),
                "lang": lang,
                "taskId": task_id,
            }),
        }
    }
}

fn run_lang_plugin(
    plugin_path: &Path,
    code: &str,
    cwd: &str,
    timeout_ms: u64,
    task_id: u64,
) -> serde_json::Value {
    let lang = plugin_path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown").to_string();
    let escaped_code = serde_json::to_string(code).unwrap_or_else(|_| "\"\"".to_string());
    let escaped_cwd = serde_json::to_string(cwd).unwrap_or_else(|_| "\"\"".to_string());
    let plugin_path_str = plugin_path.to_string_lossy();
    let bun_script = format!(
        "const p=require({});Promise.resolve(p.exec.run({},{})).then(o=>process.stdout.write(String(o||''))).catch(e=>{{process.stderr.write(String(e));process.exit(1)}})",
        serde_json::to_string(&*plugin_path_str).unwrap_or_else(|_| format!("\"{}\"", plugin_path_str)),
        escaped_code,
        escaped_cwd,
    );

    let mut cmd = Command::new("bun");
    cmd.arg("-e").arg(&bun_script)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                return serde_json::json!({
                    "ok": false,
                    "error": "bun not found",
                    "lang": lang,
                    "taskId": task_id,
                });
            }
            return serde_json::json!({
                "ok": false,
                "error": format!("spawn failed: {}", e),
                "lang": lang,
                "taskId": task_id,
            });
        }
    };

    let deadline = Duration::from_millis(timeout_ms.saturating_add(5_000));
    let start = std::time::Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let exit_code = status.code().unwrap_or(1);
                let mut stdout_buf = Vec::new();
                let mut stderr_buf = Vec::new();
                if let Some(mut s) = child.stdout.take() { let _ = s.read_to_end(&mut stdout_buf); }
                if let Some(mut s) = child.stderr.take() { let _ = s.read_to_end(&mut stderr_buf); }
                let combined = format!(
                    "{}{}",
                    String::from_utf8_lossy(&stdout_buf),
                    String::from_utf8_lossy(&stderr_buf)
                );
                return serde_json::json!({
                    "ok": exit_code == 0,
                    "output": combined,
                    "exitCode": exit_code,
                    "lang": lang,
                    "taskId": task_id,
                });
            }
            Ok(None) => {
                if start.elapsed() >= deadline {
                    let _ = child.kill();
                    let mut stdout_buf = Vec::new();
                    let mut stderr_buf = Vec::new();
                    if let Some(mut s) = child.stdout.take() { let _ = s.read_to_end(&mut stdout_buf); }
                    if let Some(mut s) = child.stderr.take() { let _ = s.read_to_end(&mut stderr_buf); }
                    let partial = format!(
                        "{}{}",
                        String::from_utf8_lossy(&stdout_buf),
                        String::from_utf8_lossy(&stderr_buf)
                    );
                    return serde_json::json!({
                        "ok": false,
                        "timedOut": true,
                        "output": format!("partial...\n{}", partial),
                        "lang": lang,
                        "taskId": task_id,
                    });
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return serde_json::json!({
                "ok": false,
                "error": format!("wait error: {}", e),
                "lang": lang,
                "taskId": task_id,
            }),
        }
    }
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
