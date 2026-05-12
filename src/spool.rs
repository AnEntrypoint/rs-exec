use std::fs;
use std::io::{BufRead, BufReader, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

static STATUS: OnceLock<Mutex<StatusState>> = OnceLock::new();

fn status() -> &'static Mutex<StatusState> {
    STATUS.get_or_init(|| Mutex::new(StatusState::new()))
}

struct StatusState {
    started_at: String,
    tasks_dispatched: u64,
    last_dispatched: Option<serde_json::Value>,
    errors_ring: std::collections::VecDeque<serde_json::Value>,
}

impl StatusState {
    fn new() -> Self {
        Self {
            started_at: iso_now(),
            tasks_dispatched: 0,
            last_dispatched: None,
            errors_ring: std::collections::VecDeque::with_capacity(8),
        }
    }
    fn record_dispatch(&mut self, id: u64, lang: &str, input_path: &str) {
        self.tasks_dispatched += 1;
        self.last_dispatched = Some(serde_json::json!({
            "id": id,
            "lang_or_verb": lang,
            "input_path": input_path,
            "at": iso_now(),
        }));
    }
    fn record_error(&mut self, msg: &str) {
        if self.errors_ring.len() >= 5 { self.errors_ring.pop_front(); }
        self.errors_ring.push_back(serde_json::json!({
            "at": iso_now(),
            "message": msg.chars().take(400).collect::<String>(),
        }));
    }
}

fn iso_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let millis = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0) % 1000) as u32;
    let day = secs / 86_400;
    let (y, mo, d) = civil_from_days(day as i64);
    let rem = secs % 86_400;
    let h = (rem / 3600) as u32;
    let mi = ((rem % 3600) / 60) as u32;
    let se = (rem % 60) as u32;
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", y, mo, d, h, mi, se, millis)
}

fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

fn count_pending_in_inbox() -> u64 {
    let in_dir = pending_dir();
    let mut n: u64 = 0;
    if let Ok(rd) = fs::read_dir(&in_dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_file() { n += 1; continue; }
            if p.is_dir() {
                if let Ok(sub) = fs::read_dir(&p) {
                    n += sub.flatten().filter(|s| s.path().is_file()).count() as u64;
                }
            }
        }
    }
    n
}

fn report_binary_version() -> String {
    std::env::var("PLUGKIT_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string())
}

fn write_status_json() {
    let path = spool_root().join(".status.json");
    let guard = match status().lock() { Ok(g) => g, Err(p) => p.into_inner() };
    let v = serde_json::json!({
        "pid": std::process::id(),
        "binary_version": report_binary_version(),
        "started_at": guard.started_at,
        "last_tick_at": iso_now(),
        "tasks_dispatched_this_session": guard.tasks_dispatched,
        "last_dispatched_task": guard.last_dispatched.clone().unwrap_or(serde_json::Value::Null),
        "pending_in_inbox": count_pending_in_inbox(),
        "errors_last_5": guard.errors_ring.iter().cloned().collect::<Vec<_>>(),
    });
    let _ = fs::write(&path, serde_json::to_string_pretty(&v).unwrap_or_default());
}

fn record_dispatch_status(id: u64, lang: &str, input_path: &str) {
    if let Ok(mut g) = status().lock() {
        g.record_dispatch(id, lang, input_path);
    }
}

fn record_error_status(msg: &str) {
    if let Ok(mut g) = status().lock() {
        g.record_error(msg);
    }
}

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
fn out_stream_path(task_id: u64) -> PathBuf { done_dir().join(format!("{}.out", task_id)) }
fn err_stream_path(task_id: u64) -> PathBuf { done_dir().join(format!("{}.err", task_id)) }
fn meta_path(task_id: u64) -> PathBuf { done_dir().join(format!("{}.json", task_id)) }
fn warn_log_path() -> PathBuf { done_dir().join(".watcher-warnings.log") }

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn truncate(p: &Path) {
    if let Some(parent) = p.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::OpenOptions::new().create(true).write(true).truncate(true).open(p);
}

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

fn stream_child_to_files(
    stdout: std::process::ChildStdout,
    stderr: std::process::ChildStderr,
    out_path: PathBuf,
    err_path: PathBuf,
) {
    truncate(&out_path);
    truncate(&err_path);
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            if let Ok(l) = line {
                if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&out_path) {
                    let _ = writeln!(f, "{}", l);
                    let _ = f.flush();
                }
            }
        }
    });
    std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            if let Ok(l) = line {
                if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&err_path) {
                    let _ = writeln!(f, "{}", l);
                    let _ = f.flush();
                }
            }
        }
    });
}

fn write_meta(
    meta_path: &Path,
    task_id: u64,
    lang: &str,
    ok: bool,
    exit_code: i32,
    started_at_ms: u128,
    timed_out: bool,
    error: Option<&str>,
) {
    let ended = now_ms();
    let mut v = serde_json::json!({
        "taskId": task_id,
        "lang": lang,
        "ok": ok,
        "exitCode": exit_code,
        "durationMs": (ended - started_at_ms) as u64,
        "timedOut": timed_out,
        "startedAt": started_at_ms as u64,
        "endedAt": ended as u64,
    });
    if let Some(e) = error {
        v["error"] = serde_json::Value::String(e.to_string());
    }
    let _ = fs::write(meta_path, v.to_string());
}

fn wait_child_write_meta(
    mut child: std::process::Child,
    meta_path: PathBuf,
    lang: String,
    task_id: u64,
    timeout_ms: u64,
    started_at_ms: u128,
) {
    let deadline = Duration::from_millis(timeout_ms.saturating_add(5_000));
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let exit_code = status.code().unwrap_or(1);
                std::thread::sleep(Duration::from_millis(100));
                write_meta(&meta_path, task_id, &lang, exit_code == 0, exit_code, started_at_ms, false, None);
                return;
            }
            Ok(None) => {
                if start.elapsed() >= deadline {
                    let _ = child.kill();
                    std::thread::sleep(Duration::from_millis(100));
                    write_meta(&meta_path, task_id, &lang, false, -1, started_at_ms, true, None);
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                write_meta(&meta_path, task_id, &lang, false, -1, started_at_ms, false, Some(&format!("wait error: {}", e)));
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
    meta_path: PathBuf,
    out_stream: PathBuf,
    err_stream: PathBuf,
    started_at_ms: u128,
) {
    if is_builtin(lang) {
        run_builtin(code_path, lang, cwd, timeout_ms, task_id, meta_path, out_stream, err_stream, started_at_ms);
    } else {
        match find_lang_plugin(lang, cwd) {
            Some(plugin_path) => run_lang_plugin(&plugin_path, code, cwd, timeout_ms, task_id, meta_path, out_stream, err_stream, started_at_ms),
            None => {
                write_meta(&meta_path, task_id, lang, false, -1, started_at_ms, false, Some(&format!("unknown lang '{}': not a builtin and no lang plugin found", lang)));
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
    meta_path: PathBuf,
    out_stream: PathBuf,
    err_stream: PathBuf,
    started_at_ms: u128,
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
            write_meta(&meta_path, task_id, lang, false, -1, started_at_ms, false, Some(&format!("spawn failed: {}", e)));
            return;
        }
    };

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    stream_child_to_files(stdout, stderr, out_stream, err_stream);
    wait_child_write_meta(child, meta_path, lang.to_string(), task_id, timeout_ms, started_at_ms);
}

fn run_lang_plugin(
    plugin_path: &Path,
    code: &str,
    cwd: &str,
    timeout_ms: u64,
    task_id: u64,
    meta_path: PathBuf,
    out_stream: PathBuf,
    err_stream: PathBuf,
    started_at_ms: u128,
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
            write_meta(&meta_path, task_id, &lang, false, -1, started_at_ms, false, Some(&err_msg));
            return;
        }
    };

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    stream_child_to_files(stdout, stderr, out_stream, err_stream);
    wait_child_write_meta(child, meta_path, lang, task_id, timeout_ms, started_at_ms);
}

const UTILITY_LANGS: &[&str] = &[
    "recall", "codesearch", "search", "memorize",
    "sleep", "status", "close",
    "runner", "type", "kill-port",
    "forget",
    "learn-status", "learn-debug", "learn-build",
    "discipline",
    "wait", "pause", "browser", "feedback",
    "health",
];

fn is_utility_lang(lang: &str) -> bool { UTILITY_LANGS.contains(&lang) }

fn which_plugkit() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PLUGKIT_BIN") {
        let pb = PathBuf::from(p);
        if pb.exists() { return Some(pb); }
    }
    if let Ok(p) = std::env::var("CLAUDE_PLUGIN_ROOT") {
        let candidate = PathBuf::from(&p).join("bin").join("plugkit");
        if candidate.exists() { return Some(candidate); }
        let candidate_exe = PathBuf::from(&p).join("bin").join("plugkit.exe");
        if candidate_exe.exists() { return Some(candidate_exe); }
    }
    if let Ok(exe) = std::env::current_exe() {
        let name = exe.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if name.contains("plugkit") { return Some(exe); }
    }
    if let Ok(p) = which::which("plugkit") { return Some(p); }
    if let Ok(exe) = std::env::current_exe() { return Some(exe); }
    None
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
        "sleep" => {
            let tid = body.split_whitespace().next().unwrap_or("0").to_string();
            vec!["sleep".into(), tid]
        }
        "status" => {
            let tid = body.split_whitespace().next().unwrap_or("").to_string();
            if tid.is_empty() { vec!["status".into()] } else { vec!["status".into(), tid] }
        }
        "close" => {
            let tid = body.split_whitespace().next().unwrap_or("0").to_string();
            vec!["close".into(), tid]
        }
        "runner" => {
            let action = body.split_whitespace().next().unwrap_or("status").to_string();
            vec!["runner".into(), action]
        }
        "type" => {
            let mut lines = body.splitn(2, '\n');
            let tid = lines.next().unwrap_or("").trim().to_string();
            let stdin = lines.next().unwrap_or("").to_string();
            let mut v = vec!["type".into(), tid];
            if !stdin.is_empty() { v.push(stdin); }
            v
        }
        "kill-port" => {
            let port = body.split_whitespace().next().unwrap_or("0").to_string();
            vec!["kill-port".into(), port]
        }
        "forget" => {
            let mut parts = body.splitn(2, char::is_whitespace);
            let kind = parts.next().unwrap_or("").trim().to_string();
            let target = parts.next().unwrap_or("").trim().to_string();
            let mut v = vec!["forget".into()];
            if !kind.is_empty() { v.push(kind); }
            if !target.is_empty() { v.push(target); }
            v
        }
        "discipline" => {
            let mut parts = body.splitn(2, char::is_whitespace);
            let sub = parts.next().unwrap_or("list").trim().to_string();
            let rest = parts.next().unwrap_or("").trim().to_string();
            let mut v = vec!["discipline".into(), sub];
            if !rest.is_empty() { v.push(rest); }
            v
        }
        "learn-status" => vec!["learn".into(), "status".into()],
        "learn-build" => vec!["learn".into(), "build-communities".into()],
        "learn-debug" => {
            let mut v = vec!["learn".into(), "debug".into()];
            if !body.is_empty() { v.push(body.to_string()); }
            v
        }
        "feedback" => {
            vec!["learn".into(), "feedback".into(), body.to_string()]
        }
        "browser" => {
            vec!["browser".into(), body.to_string()]
        }
        "health" => {
            vec!["health".into()]
        }
        _ => vec![verb.into(), body.to_string()],
    }
}

fn run_plugkit_verb(verb: &str, content: &str, task_id: u64) {
    let meta = meta_path(task_id);
    let out_stream = out_stream_path(task_id);
    let err_stream = err_stream_path(task_id);
    let _ = fs::create_dir_all(done_dir());
    let started = now_ms();

    let exe = match which_plugkit() {
        Some(p) => p,
        None => {
            write_meta(&meta, task_id, verb, false, -1, started, false, Some("plugkit not found in PATH"));
            return;
        }
    };

    let args = build_plugkit_args(verb, content);
    let verb_owned = verb.to_string();

    std::thread::spawn(move || {
        run_subprocess_streaming(&exe, &args, &verb_owned, task_id, 300_000, meta, out_stream, err_stream, started);
    });
}

fn run_subprocess_streaming(
    exe: &Path,
    args: &[String],
    lang: &str,
    task_id: u64,
    timeout_ms: u64,
    meta_path: PathBuf,
    out_stream: PathBuf,
    err_stream: PathBuf,
    started_at_ms: u128,
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
            write_meta(&meta_path, task_id, lang, false, -1, started_at_ms, false, Some(&format!("spawn failed: {}", e)));
            return;
        }
    };

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    stream_child_to_files(stdout, stderr, out_stream, err_stream);
    wait_child_write_meta(child, meta_path, lang.to_string(), task_id, timeout_ms, started_at_ms);
}

fn run_request_raw(path: &Path, lang: String, task_id: u64) {
    let code = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return,
    };
    let _ = fs::remove_file(path);

    if is_utility_lang(&lang) {
        let meta = meta_path(task_id);
        let out_stream = out_stream_path(task_id);
        let _ = fs::create_dir_all(done_dir());
        let started = now_ms();
        match lang.as_str() {
            "wait" => {
                let secs: u64 = code.trim().parse().unwrap_or(0);
                if secs == 0 {
                    write_meta(&meta, task_id, "wait", false, -1, started, false, Some("exec:wait requires <seconds>"));
                    return;
                }
                let secs = secs.min(3600);
                std::thread::spawn(move || {
                    truncate(&out_stream);
                    std::thread::sleep(Duration::from_secs(secs));
                    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&out_stream) {
                        let _ = writeln!(f, "slept {}s", secs);
                    }
                    write_meta(&meta, task_id, "wait", true, 0, started, false, None);
                });
                return;
            }
            "pause" => {
                write_meta(&meta, task_id, "pause", false, -1, started, false, Some("exec:pause via spool not yet wired — pause currently mutates .gm/prd.yml via Bash hook path. Residual: spool dispatch for pause pending."));
                return;
            }
            _ => {}
        }
        run_plugkit_verb(&lang, &code, task_id);
        return;
    }

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());
    let timeout_ms: u64 = 300_000;

    let meta = meta_path(task_id);
    let out_stream = out_stream_path(task_id);
    let err_stream = err_stream_path(task_id);
    let _ = fs::create_dir_all(done_dir());
    let started = now_ms();
    let work_dir = spool_root().join("work");
    let _ = fs::create_dir_all(&work_dir);
    let code_path = work_dir.join(format!("{}.code", task_id));
    let _ = fs::write(&code_path, &code);

    let code_path_clone = code_path.clone();
    std::thread::spawn(move || {
        execute_task_streaming(&code, &lang, &cwd, &code_path_clone, timeout_ms, task_id, meta, out_stream, err_stream, started);
        let _ = fs::remove_file(&code_path_clone);
    });
}

fn ext_to_lang(ext: &str) -> Option<&'static str> {
    match ext.to_lowercase().as_str() {
        "js" | "mjs" | "cjs" => Some("nodejs"),
        "py" => Some("python"),
        "sh" | "bash" => Some("bash"),
        "ts" => Some("typescript"),
        "go" => Some("go"),
        "rs" => Some("rust"),
        "c" => Some("c"),
        "cpp" | "cc" | "cxx" => Some("cpp"),
        "java" => Some("java"),
        "ps1" => Some("powershell"),
        "cmd" | "bat" => Some("cmd"),
        _ => None,
    }
}

fn dispatch_entry(p: &Path) {
    let components: Vec<_> = p.components().collect();
    let n = components.len();

    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");

    if n >= 2 {
        let parent_name = components[n - 2].as_os_str().to_string_lossy().to_string();
        if parent_name != "in" {
            let lang_from_dir = parent_name.to_lowercase();
            if let Ok(task_id) = stem.parse::<u64>() {
                record_dispatch_status(task_id, &lang_from_dir, &p.to_string_lossy());
                run_request_raw(p, lang_from_dir, task_id);
                return;
            }
        } else if let Some(lang_from_ext) = ext_to_lang(ext) {
            if let Ok(task_id) = stem.parse::<u64>() {
                record_dispatch_status(task_id, lang_from_ext, &p.to_string_lossy());
                run_request_raw(p, lang_from_ext.to_string(), task_id);
                return;
            }
        } else {
            let warn_path = warn_log_path();
            let _ = fs::create_dir_all(done_dir());
            let warn_msg = format!(
                "ignored stray file at in/ root (unknown ext; use in/<lang>/{}.<ext>): {}",
                stem,
                p.display()
            );
            if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&warn_path) {
                let _ = writeln!(
                    f,
                    "[{}] {}",
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
                    warn_msg
                );
            }
            record_error_status(&warn_msg);
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
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("watcher panic: {}", info);
        record_error_status(&msg);
        let _ = fs::write(spool_root().join(".watcher-panic.log"), &msg);
    }));
    loop {
        let tick = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            watch_once();
            let _ = fs::write(&hb_path, format!(
                "{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
            ));
            write_status_json();
        }));
        if let Err(e) = tick {
            let msg = if let Some(s) = e.downcast_ref::<String>() { s.clone() }
                else if let Some(s) = e.downcast_ref::<&'static str>() { (*s).to_string() }
                else { "watcher tick panic (non-string payload)".to_string() };
            record_error_status(&format!("tick panic survived: {}", msg));
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}
