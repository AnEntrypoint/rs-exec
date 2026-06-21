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

fn write_result(task_id: u64, stdout: &str, stderr: &str, exit_code: i32, timed_out: bool) {
    let ended_at = wasm_host::now_ms();
    let body = serde_json::json!({
        "taskId": task_id,
        "ok": exit_code == 0,
        "exitCode": exit_code,
        "stdout": stdout,
        "stderr": stderr,
        "timedOut": timed_out,
        "endedAt": ended_at,
    });
    let key = format!("{}", task_id);
    let s = body.to_string();
    if !wasm_host::kv_put("outbox", &key, s.as_bytes()) {
        wasm_host::log(&format!("write_result: kv_put failed for task_id={}", task_id));
    }
}

pub fn dispatch_pending() -> u32 {
    let raw = wasm_host::kv_query("inbox", "");
    if raw.is_empty() { return 0; }
    let s = match std::str::from_utf8(&raw) { Ok(v) => v, Err(_) => return 0 };
    let raw_tasks: Vec<serde_json::Value> = match serde_json::from_str(s) {
        Ok(v) => v,
        Err(e) => {
            wasm_host::log(&format!("dispatch_pending: malformed inbox batch, 0 dispatched: {}", e));
            return 0;
        }
    };
    let mut n = 0u32;
    for raw_task in &raw_tasks {
        match serde_json::from_value::<InboxTask>(raw_task.clone()) {
            Ok(t) => { dispatch_one(&t); n += 1; }
            Err(e) => {
                wasm_host::log(&format!("dispatch_pending: malformed task skipped: {}", e));
            }
        }
    }
    n
}

const MIN_TIMEOUT_MS: u64 = 100;
const MAX_TIMEOUT_MS: u64 = 600_000;

fn dispatch_one(t: &InboxTask) {
    let lang_l = t.lang.to_ascii_lowercase();
    let normalized = match lang_l.as_str() {
        "nodejs" | "javascript" | "node" | "js" => "nodejs",
        "python" | "py" => "python",
        "bash" | "sh" | "shell" | "zsh" => "bash",
        "powershell" | "ps1" => "powershell",
        "go" | "golang" => "go",
        "rust" | "rs" => "rust",
        "c" => "c",
        "cpp" | "c++" | "cxx" => "cpp",
        "java" => "java",
        "typescript" | "ts" => "typescript",
        "deno" => "deno",
        "ssh" => "ssh",
        other => other,
    };
    if t.timeout_ms == 0 {
        let body = serde_json::json!({
            "ok": false,
            "error": "missing timeoutMs",
            "required": "positive integer milliseconds",
        });
        write_result(t.task_id, "", &body.to_string(), 1, false);
        return;
    }
    if t.timeout_ms < MIN_TIMEOUT_MS {
        let body = serde_json::json!({
            "ok": false,
            "error": "timeoutMs below floor",
            "min": MIN_TIMEOUT_MS,
            "received": t.timeout_ms,
        });
        write_result(t.task_id, "", &body.to_string(), 1, false);
        return;
    }
    let timeout_ms = t.timeout_ms.min(MAX_TIMEOUT_MS);
    let opts = serde_json::json!({
        "taskId": t.task_id,
        "cwd": t.cwd,
        "timeoutMs": timeout_ms,
        "lang": normalized,
    }).to_string();
    let result = wasm_host::exec_js(&t.code, &opts);
    write_result(t.task_id, &result.stdout, &result.stderr, result.exit_code, result.timed_out);
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
