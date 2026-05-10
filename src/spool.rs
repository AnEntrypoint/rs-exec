use std::fs;
use std::io::{BufRead, BufReader, Write as IoWrite};
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

fn pending_dir() -> PathBuf { spool_root().join("in") }
fn done_dir() -> PathBuf { spool_root().join("out") }
fn log_dir() -> PathBuf { spool_root().join("log") }

fn find_lang_plugin(lang: &str, cwd: &str) -> Option<PathBuf> {
    let filename = format!("{}.js", lang);
    let project_plugin = PathBuf::from(cwd).join("lang").join(&filename);
    if project_plugin.exists() { return Some(project_plugin); }
    if let Ok(plugin_root) = std::env::var("CLAUDE_PLUGIN_ROOT") {
        let global_plugin = PathBuf::from(plugin_root).join("lang").join(&filename);
        if global_plugin.exists() { return Some(global_plugin); }
    }
    None
}

fn is_builtin(lang: &str) -> bool { BUILTIN_LANGS.contains(&lang) }

fn stream_child_to_log(
    stdout: std::process::ChildStdout,
    stderr: std::process::ChildStderr,
    log_path: PathBuf,
) {
    let log_path_err = log_path.clone();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            if let Ok(l) = line {
                if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&log_path) {
                    let _ = writeln!(f, "{}", l);
                }
            }
        }
    });
    std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            if let Ok(l) = line {
                if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&log_path_err) {
                    let _ = writeln!(f, "{}", l);
                }
            }
        }
    });
}

fn wait_child_write_out(
    mut child: std::process::Child,
    out_path: PathBuf,
    log_path: PathBuf,
    lang: String,
    task_id: u64,
    timeout_ms: u64,
) {
    let deadline = Duration::from_millis(timeout_ms.saturating_add(5_000));
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let exit_code = status.code().unwrap_or(1);
                std::thread::sleep(Duration::from_millis(100));
                let output = fs::read_to_string(&log_path).unwrap_or_default();
                let _ = fs::write(&out_path, serde_json::json!({
                    "ok": exit_code == 0,
                    "output": output,
                    "exitCode": exit_code,
                    "lang": lang,
                    "taskId": task_id,
                }).to_string());
                return;
            }
            Ok(None) => {
                if start.elapsed() >= deadline {
                    let _ = child.kill();
                    std::thread::sleep(Duration::from_millis(100));
                    let output = fs::read_to_string(&log_path).unwrap_or_default();
                    let _ = fs::write(&out_path, serde_json::json!({
                        "ok": false,
                        "timedOut": true,
                        "output": output,
                        "lang": lang,
                        "taskId": task_id,
                    }).to_string());
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                let _ = fs::write(&out_path, serde_json::json!({
                    "ok": false,
                    "error": format!("wait error: {}", e),
                    "lang": lang,
                    "taskId": task_id,
                }).to_string());
                return;
            }
        }
    }
}

fn execute_task_streaming(
    code: &str,
    lang: &str,
    cwd: &str,
    code_path: &Path,
    timeout_ms: u64,
    task_id: u64,
    out_path: PathBuf,
    log_path: PathBuf,
) {
    if is_builtin(lang) {
        run_builtin(code_path, lang, cwd, timeout_ms, task_id, out_path, log_path);
    } else {
        match find_lang_plugin(lang, cwd) {
            Some(plugin_path) => run_lang_plugin(&plugin_path, code, cwd, timeout_ms, task_id, out_path, log_path),
            None => {
                let _ = fs::write(&out_path, serde_json::json!({
                    "ok": false,
                    "error": format!("unknown lang '{}': not a builtin and no lang plugin found", lang),
                    "lang": lang,
                    "taskId": task_id,
                }).to_string());
            }
        }
    }
}

fn run_builtin(
    code_path: &Path,
    lang: &str,
    cwd: &str,
    timeout_ms: u64,
    task_id: u64,
    out_path: PathBuf,
    log_path: PathBuf,
) {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("rs-exec"));
    let mut cmd = Command::new(&exe);
    cmd.arg("exec")
        .arg("--lang").arg(lang)
        .arg("--cwd").arg(cwd)
        .arg("--session").arg(format!("spool-{}", task_id))
        .arg("--timeout-ms").arg(timeout_ms.to_string())
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
        Err(e) => {
            let _ = fs::write(&out_path, serde_json::json!({
                "ok": false, "error": format!("spawn failed: {}", e), "lang": lang, "taskId": task_id,
            }).to_string());
            return;
        }
    };

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    stream_child_to_log(stdout, stderr, log_path.clone());
    wait_child_write_out(child, out_path, log_path, lang.to_string(), task_id, timeout_ms);
}

fn run_lang_plugin(
    plugin_path: &Path,
    code: &str,
    cwd: &str,
    timeout_ms: u64,
    task_id: u64,
    out_path: PathBuf,
    log_path: PathBuf,
) {
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
            let err_msg = if e.kind() == std::io::ErrorKind::NotFound { "bun not found".to_string() } else { format!("spawn failed: {}", e) };
            let _ = fs::write(&out_path, serde_json::json!({
                "ok": false, "error": err_msg, "lang": lang, "taskId": task_id,
            }).to_string());
            return;
        }
    };

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    stream_child_to_log(stdout, stderr, log_path.clone());
    wait_child_write_out(child, out_path, log_path, lang, task_id, timeout_ms);
}

const UTILITY_LANGS: &[&str] = &["recall", "codesearch", "memorize"];

fn is_utility_lang(lang: &str) -> bool { UTILITY_LANGS.contains(&lang) }

fn which_plugkit() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("CLAUDE_PLUGIN_ROOT") {
        let candidate = PathBuf::from(&p).join("bin").join("plugkit");
        if candidate.exists() { return Some(candidate); }
        let candidate_exe = PathBuf::from(&p).join("bin").join("plugkit.exe");
        if candidate_exe.exists() { return Some(candidate_exe); }
    }
    which::which("plugkit").ok()
}

fn build_plugkit_args(verb: &str, content: &str) -> Vec<String> {
    let body = content.trim();
    match verb {
        "recall" => {
            let query = body.replace('\n', " ");
            vec!["recall".into(), "--limit".into(), "5".into(), query]
        }
        "memorize" => {
            let (source, fact) = if let Some(nl) = body.find('\n') {
                let first = body[..nl].trim();
                if !first.is_empty() && first.len() < 64 && !first.contains(' ') {
                    (first.to_string(), body[nl+1..].trim().to_string())
                } else {
                    ("memorize".to_string(), body.to_string())
                }
            } else {
                ("memorize".to_string(), body.to_string())
            };
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let tmp = std::env::temp_dir().join(format!("spool-memorize-{}.txt", ts));
            let _ = fs::write(&tmp, &fact);
            vec!["memorize".into(), "--source".into(), source, "--file".into(), tmp.to_string_lossy().to_string()]
        }
        "codesearch" | "search" => {
            let query = body.replace('\n', " ");
            vec!["search".into(), query]
        }
        _ => vec![verb.into(), body.to_string()],
    }
}

fn run_plugkit_verb(verb: &str, content: &str, task_id: u64) {
    let out_path = done_dir().join(format!("{}.json", task_id));
    let log_path = log_dir().join(format!("{}.log", task_id));
    let _ = fs::create_dir_all(done_dir());
    let _ = fs::create_dir_all(log_dir());
    let _ = fs::write(&log_path, "");

    let exe = match which_plugkit() {
        Some(p) => p,
        None => {
            let _ = fs::write(&out_path, serde_json::json!({
                "ok": false, "error": "plugkit not found in PATH", "lang": verb, "taskId": task_id
            }).to_string());
            return;
        }
    };

    let args = build_plugkit_args(verb, content);
    let verb_owned = verb.to_string();

    std::thread::spawn(move || {
        run_subprocess_streaming(&exe, &args, &verb_owned, task_id, 300_000, out_path, log_path);
    });
}

fn run_subprocess_streaming(
    exe: &Path,
    args: &[String],
    lang: &str,
    task_id: u64,
    timeout_ms: u64,
    out_path: PathBuf,
    log_path: PathBuf,
) {
    let mut cmd = Command::new(exe);
    for a in args { cmd.arg(a); }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = fs::write(&out_path, serde_json::json!({
                "ok": false, "error": format!("spawn failed: {}", e), "lang": lang, "taskId": task_id
            }).to_string());
            return;
        }
    };

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    stream_child_to_log(stdout, stderr, log_path.clone());
    wait_child_write_out(child, out_path, log_path, lang.to_string(), task_id, timeout_ms);
}

fn run_request_raw(path: &Path, lang: String, task_id: u64) {
    let code = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return,
    };
    let _ = fs::remove_file(path);

    if is_utility_lang(&lang) {
        run_plugkit_verb(&lang, &code, task_id);
        return;
    }

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());
    let timeout_ms: u64 = 300_000;

    let out_path = done_dir().join(format!("{}.json", task_id));
    let log_path = log_dir().join(format!("{}.log", task_id));
    let _ = fs::create_dir_all(done_dir());
    let _ = fs::create_dir_all(log_dir());
    let _ = fs::write(&log_path, "");
    let code_path = pending_dir().join(format!("{}.code", task_id));
    let _ = fs::write(&code_path, &code);

    let code_path_clone = code_path.clone();
    std::thread::spawn(move || {
        execute_task_streaming(&code, &lang, &cwd, &code_path_clone, timeout_ms, task_id, out_path, log_path);
        let _ = fs::remove_file(&code_path_clone);
    });
}

fn dispatch_entry(p: &Path) {
    let components: Vec<_> = p.components().collect();
    let n = components.len();

    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");

    if n >= 2 {
        let parent_name = components[n - 2].as_os_str().to_string_lossy().to_string();
        if parent_name != "in" {
            let lang_from_dir = parent_name.to_lowercase();
            if let Ok(task_id) = stem.parse::<u64>() {
                run_request_raw(p, lang_from_dir, task_id);
                return;
            }
        } else {
            let warn_path = log_dir().join("watcher-warnings.log");
            let _ = fs::create_dir_all(log_dir());
            if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&warn_path) {
                let _ = writeln!(
                    f,
                    "[{}] ignored stray file at in/ root (JSON form removed; use in/<lang>/{}.<ext>): {}",
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
                    stem,
                    p.display()
                );
            }
            let _ = fs::remove_file(p);
        }
    }
}

fn file_is_stable(p: &Path) -> bool {
    let md = match fs::metadata(p) { Ok(m) => m, Err(_) => return false };
    let mtime = match md.modified() { Ok(t) => t, Err(_) => return false };
    match std::time::SystemTime::now().duration_since(mtime) {
        Ok(age) => age >= Duration::from_millis(250),
        Err(_) => false,
    }
}

pub fn watch_once() {
    let _ = fs::create_dir_all(pending_dir());
    let _ = fs::create_dir_all(done_dir());
    let _ = fs::create_dir_all(log_dir());
    if let Ok(rd) = fs::read_dir(pending_dir()) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_file() {
                if file_is_stable(&p) { dispatch_entry(&p); }
            } else if p.is_dir() {
                if let Ok(sub) = fs::read_dir(&p) {
                    for sub_entry in sub.flatten() {
                        let sp = sub_entry.path();
                        if sp.is_file() && file_is_stable(&sp) { dispatch_entry(&sp); }
                    }
                }
            }
        }
    }
}

pub fn run_daemon() {
    let root = spool_root();
    let _ = fs::create_dir_all(&root);
    let pid_path = root.join(".watcher.pid");
    let hb_path = root.join(".watcher.heartbeat");
    let _ = fs::write(&pid_path, std::process::id().to_string());
    loop {
        watch_once();
        let _ = fs::write(&hb_path, format!(
            "{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        ));
        std::thread::sleep(Duration::from_millis(300));
    }
}
