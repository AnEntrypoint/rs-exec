use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH, Instant};
use serde_json::{json, Value};

const ENV_DISABLE: &str = "GM_LOG_DISABLE";
const ENV_DIR: &str = "GM_LOG_DIR";

pub fn root_dir() -> PathBuf {
    if let Ok(p) = std::env::var(ENV_DIR) {
        if !p.is_empty() { return PathBuf::from(p); }
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    PathBuf::from(home).join(".claude").join("gm-log")
}

fn enabled() -> bool {
    std::env::var_os(ENV_DISABLE).is_none()
}

fn today_dir() -> Option<PathBuf> {
    if !enabled() { return None; }
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let day = ymd_utc(secs);
    let dir = root_dir().join(day);
    fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn ymd_utc(unix_secs: u64) -> String {
    let secs = unix_secs as i64;
    let days_since_epoch = secs.div_euclid(86_400);
    let (y, m, d) = civil_from_days(days_since_epoch);
    format!("{:04}-{:02}-{:02}", y, m, d)
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

fn iso_now() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let millis = (SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0) % 1000) as u32;
    let day = secs / 86_400;
    let (y, mo, d) = civil_from_days(day as i64);
    let rem = secs % 86_400;
    let h = rem / 3600;
    let mi = (rem % 3600) / 60;
    let s = rem % 60;
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", y, mo, d, h, mi, s, millis)
}

fn pid() -> u32 { std::process::id() }

fn session_id() -> String {
    std::env::var("CLAUDE_SESSION_ID")
        .or_else(|_| std::env::var("GM_SESSION_ID"))
        .unwrap_or_default()
}

static APPEND_LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();

fn append_jsonl(file: &PathBuf, line: &str) {
    let lock = APPEND_LOCK.get_or_init(|| std::sync::Mutex::new(()));
    let _g = lock.lock();
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(file) {
        let _ = f.write_all(line.as_bytes());
        let _ = f.write_all(b"\n");
    }
    let _ = File::open(file).map(|f| drop(f));
}

pub fn event(subsystem: &str, event: &str, mut fields: Value) {
    let dir = match today_dir() { Some(d) => d, None => return };
    let mut base = json!({
        "ts": iso_now(),
        "sub": subsystem,
        "event": event,
        "pid": pid(),
    });
    let sid = session_id();
    if !sid.is_empty() { base["sess"] = json!(sid); }
    if let Some(obj) = fields.as_object_mut() {
        if let Some(b) = base.as_object_mut() {
            for (k, v) in obj.drain() { b.insert(k, v); }
        }
    } else if !fields.is_null() {
        if let Some(b) = base.as_object_mut() { b.insert("data".into(), fields); }
    }
    let line = base.to_string();
    let file = dir.join(format!("{}.jsonl", subsystem.replace(['/', '\\'], "_")));
    append_jsonl(&file, &line);
}

pub fn span<F, R>(subsystem: &str, name: &str, fields: Value, f: F) -> R
where F: FnOnce() -> R {
    let start = Instant::now();
    let mut start_fields = json!({ "phase": "start" });
    if let (Some(sf), Some(extra)) = (start_fields.as_object_mut(), fields.as_object()) {
        for (k, v) in extra { sf.insert(k.clone(), v.clone()); }
    }
    event(subsystem, name, start_fields);
    let result = f();
    let dur_ms = start.elapsed().as_millis() as u64;
    let mut end_fields = json!({ "phase": "end", "dur_ms": dur_ms });
    if let (Some(ef), Some(extra)) = (end_fields.as_object_mut(), fields.as_object()) {
        for (k, v) in extra { ef.insert(k.clone(), v.clone()); }
    }
    event(subsystem, name, end_fields);
    result
}
