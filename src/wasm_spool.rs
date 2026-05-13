#![allow(dead_code)]
use crate::wasm_host;
use serde::Deserialize;

#[derive(Deserialize)]
struct InboxTask {
    #[serde(rename = "taskId")]
    task_id: u64,
    lang: String,
    code: String,
    #[serde(default)]
    cwd: String,
    #[serde(default, rename = "timeoutMs")]
    timeout_ms: u64,
}

const REJECTED: &[&str] = &["python","py","bash","sh","shell","zsh","ssh","runner","type","kill-port","powershell","ps1","go","rust","c","cpp","java","deno"];

fn is_rejected(lang: &str) -> bool { REJECTED.contains(&lang) }

fn write_result(task_id: u64, stdout: &str, stderr: &str, exit_code: i32, timed_out: bool) {
    let started = wasm_host::now_ms();
    let body = serde_json::json!({
        "taskId": task_id,
        "ok": exit_code == 0,
        "exitCode": exit_code,
        "stdout": stdout,
        "stderr": stderr,
        "timedOut": timed_out,
        "endedAt": started,
    });
    let key = format!("{}", task_id);
    let s = body.to_string();
    wasm_host::kv_put("outbox", &key, s.as_bytes());
}

pub fn dispatch_pending() -> u32 {
    let raw = wasm_host::kv_query("inbox", "");
    if raw.is_empty() { return 0; }
    let s = match std::str::from_utf8(&raw) { Ok(v) => v, Err(_) => return 0 };
    let tasks: Vec<InboxTask> = match serde_json::from_str(s) { Ok(v) => v, Err(_) => return 0 };
    let mut n = 0u32;
    for t in &tasks {
        dispatch_one(t);
        n += 1;
    }
    n
}

fn dispatch_one(t: &InboxTask) {
    let lang_l = t.lang.to_ascii_lowercase();
    let normalized = match lang_l.as_str() {
        "nodejs" | "javascript" | "node" | "js" => "nodejs",
        other => other,
    };
    if is_rejected(normalized) {
        let msg = format!("language unavailable in browser: {}", t.lang);
        write_result(t.task_id, "", &format!("--- stderr ---\n{}\n", msg), 1, false);
        wasm_host::log(&format!("[wasm-spool] rejected task {} lang={}", t.task_id, t.lang));
        return;
    }
    if normalized != "nodejs" {
        let msg = format!("language unavailable in browser: {}", t.lang);
        write_result(t.task_id, "", &format!("--- stderr ---\n{}\n", msg), 1, false);
        return;
    }
    let opts = serde_json::json!({
        "taskId": t.task_id,
        "cwd": t.cwd,
        "timeoutMs": if t.timeout_ms == 0 { 300_000u64 } else { t.timeout_ms },
    }).to_string();
    let (status, out) = wasm_host::exec_js(&t.code, &opts);
    let stdout = std::str::from_utf8(&out).unwrap_or("").to_string();
    let exit_code = if status == 0 { 0 } else { status as i32 };
    write_result(t.task_id, &stdout, "", exit_code, false);
}

pub fn execute(task_json: &str) -> u32 {
    let task: InboxTask = match serde_json::from_str(task_json) { Ok(v) => v, Err(_) => return 1 };
    dispatch_one(&task);
    0
}

#[no_mangle]
pub extern "C" fn rs_exec_dispatch_pending() -> u32 {
    dispatch_pending()
}

#[no_mangle]
pub extern "C" fn rs_exec_execute(ptr: *const u8, len: u32) -> u32 {
    let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    let s = match std::str::from_utf8(slice) { Ok(v) => v, Err(_) => return 1 };
    execute(s)
}
