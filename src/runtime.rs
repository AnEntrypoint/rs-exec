#![allow(dead_code)]
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use tempfile::TempDir;
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

pub fn normalize_cwd(cwd: &str) -> String {
    if cfg!(windows) {
        if let Some(rest) = cwd.strip_prefix('/') {
            let mut chars = rest.chars();
            if let (Some(drive), Some(sep)) = (chars.next(), chars.next()) {
                if drive.is_ascii_alphabetic() && sep == '/' {
                    return format!("{}:/{}", drive.to_ascii_uppercase(), chars.as_str());
                }
            }
        }
    }
    cwd.to_string()
}

fn find_bin(candidates: &[&str]) -> String {
    for &b in candidates {
        if let Ok(path) = which::which(b) {
            return path.to_string_lossy().to_string();
        }
    }
    candidates[0].to_string()
}

static PYTHON: OnceLock<String> = OnceLock::new();
static BASH: OnceLock<String> = OnceLock::new();
static DENO: OnceLock<String> = OnceLock::new();
static GO: OnceLock<String> = OnceLock::new();
static RUSTC: OnceLock<String> = OnceLock::new();
static GCC: OnceLock<String> = OnceLock::new();
static GPP: OnceLock<String> = OnceLock::new();
static JAVA: OnceLock<String> = OnceLock::new();
static JAVAC: OnceLock<String> = OnceLock::new();
static POWERSHELL: OnceLock<String> = OnceLock::new();
static PLAYWRITER: OnceLock<String> = OnceLock::new();

fn python() -> &'static str { PYTHON.get_or_init(|| find_bin(&["python3", "python"])) }
fn bash() -> &'static str { BASH.get_or_init(|| find_bin(&["bash", "sh"])) }
fn deno() -> &'static str { DENO.get_or_init(|| find_bin(&["deno"])) }
fn go() -> &'static str { GO.get_or_init(|| find_bin(&["go"])) }
fn rustc() -> &'static str { RUSTC.get_or_init(|| find_bin(&["rustc"])) }
fn gcc() -> &'static str { GCC.get_or_init(|| find_bin(&["gcc"])) }
fn gpp() -> &'static str { GPP.get_or_init(|| find_bin(&["g++"])) }
fn java() -> &'static str { JAVA.get_or_init(|| find_bin(&["java"])) }
fn javac() -> &'static str { JAVAC.get_or_init(|| find_bin(&["javac"])) }
fn powershell() -> &'static str { POWERSHELL.get_or_init(|| find_bin(&["pwsh", "powershell"])) }
fn ensure_playwriter() -> Option<PathBuf> {
    if let Ok(p) = which::which("playwriter") {
        return Some(p);
    }
    eprintln!("[browser] playwriter not found on PATH — installing globally...");
    let installer = which::which("npm")
        .or_else(|_| which::which("bun"))
        .or_else(|_| which::which("pnpm"));
    let installer = match installer {
        Ok(p) => p,
        Err(_) => {
            eprintln!("[browser] Neither npm, bun, nor pnpm found — cannot auto-install playwriter.");
            return None;
        }
    };
    let is_bun = installer.file_stem().and_then(|s| s.to_str()) == Some("bun");
    let args: Vec<&str> = if is_bun {
        vec!["add", "-g", "playwriter"]
    } else {
        vec!["install", "-g", "playwriter"]
    };
    let mut cmd = Command::new(&installer);
    cmd.args(&args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    #[cfg(windows)]
    { cmd.creation_flags(CREATE_NO_WINDOW); }
    match cmd.status() {
        Ok(s) if s.success() => {
            eprintln!("[browser] playwriter installed via {}.", installer.display());
            which::which("playwriter").ok()
        }
        Ok(s) => {
            eprintln!("[browser] playwriter install exited with status {}.", s);
            None
        }
        Err(e) => {
            eprintln!("[browser] playwriter install spawn failed: {}", e);
            None
        }
    }
}

fn playwriter() -> &'static str {
    PLAYWRITER.get_or_init(|| {
        if let Some(p) = ensure_playwriter() {
            let dir = p.parent().unwrap_or(std::path::Path::new("."));
            let bin_js = dir.join("node_modules").join("playwriter").join("bin.js");
            if bin_js.exists() { return bin_js.to_string_lossy().to_string(); }
            return p.to_string_lossy().to_string();
        }
        "playwriter".to_string()
    })
}

fn strip_quotes(s: &str) -> String {
    let s = s.trim();
    if (s.starts_with('\'') && s.ends_with('\'')) || (s.starts_with('"') && s.ends_with('"')) {
        s[1..s.len()-1].to_string()
    } else {
        s.to_string()
    }
}

fn split_playwriter_args(rest: &str) -> Vec<String> {
    if let Some(e_pos) = rest.find(" -e ") {
        let before = &rest[..e_pos];
        let after = rest[e_pos + 4..].trim();
        let mut args: Vec<String> = shlex::split(before).unwrap_or_else(|| before.split_whitespace().map(str::to_string).collect());
        args.push("-e".to_string());
        args.push(strip_quotes(after));
        return args;
    }
    shlex::split(rest).unwrap_or_else(|| rest.split_whitespace().map(str::to_string).collect())
}

pub struct SpawnResult {
    pub child: Child,
    pub _tmpdir: Option<TempDir>,
    pub compile_phase: Option<CompilePhase>,
}

pub struct CompilePhase {
    pub bin_path: PathBuf,
    pub runtime: String,
    pub cp: Option<String>,
    pub class_name: Option<String>,
    pub cwd: String,
    pub _tmpdir: TempDir,
}


pub fn spawn_process(runtime: &str, code: &str, cwd_raw: &str, session_id: &str) -> anyhow::Result<SpawnResult> {
    let cwd_owned = normalize_cwd(cwd_raw);
    let cwd = cwd_owned.as_str();
    ensure_plugkit_gitignore(cwd);
    match runtime {
        "nodejs" | "typescript" => {
            let bun_bin = which::which("bun.exe")
                .or_else(|_| which::which("bun"))
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| "bun".to_string());
            let child = spawn_no_window(Command::new(&bun_bin)
                .args(["-e", code])
                .current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()))?;
            Ok(SpawnResult { child, _tmpdir: None, compile_phase: None })
        }
        "python" => {
            let child = spawn_no_window(Command::new(python())
                .args(["-c", code])
                .current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()))?;
            Ok(SpawnResult { child, _tmpdir: None, compile_phase: None })
        }
        "powershell" => {
            let child = spawn_no_window(Command::new(powershell())
                .args(["-NoProfile", "-NonInteractive", "-Command", code])
                .current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()))?;
            Ok(SpawnResult { child, _tmpdir: None, compile_phase: None })
        }
        "cmd" => {
            let child = spawn_no_window(Command::new("cmd.exe")
                .args(["/c", code])
                .current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()))?;
            Ok(SpawnResult { child, _tmpdir: None, compile_phase: None })
        }
        "bash" => {
            let tmp = tempfile::tempdir()?;
            let real_bash_on_win: Option<String> = if cfg!(windows) {
                which::which("bash").ok().map(|p| p.to_string_lossy().to_string())
                    .or_else(|| {
                        let candidates = [
                            r"C:\Program Files\Git\bin\bash.exe",
                            r"C:\Program Files\Git\usr\bin\bash.exe",
                            r"C:\Program Files (x86)\Git\bin\bash.exe",
                        ];
                        candidates.iter().find(|p| std::path::Path::new(p).exists()).map(|s| s.to_string())
                    })
            } else { None };

            let (cmd, script_content, ext, args): (String, String, &str, Vec<String>) = if cfg!(windows) {
                if let Some(b) = real_bash_on_win {
                    let script = tmp.path().join("script.sh");
                    let content = format!("#!/bin/bash\nset -e\n{}", code);
                    std::fs::write(&script, &content)?;
                    (b, content, ".sh", vec![script.to_string_lossy().replace('\\', "/")])
                } else {
                    eprintln!("[exec:bash] real bash not found on Windows — falling back to PowerShell. POSIX shell syntax ([ -z ], &&/||, if/then/fi, `command`) will NOT work. Install git-bash or use exec:powershell / exec:nodejs.");
                    let script = tmp.path().join("script.ps1");
                    let content = format!("$ErrorActionPreference = 'Continue'\n{}", code);
                    std::fs::write(&script, &content)?;
                    (powershell().to_string(), content, ".ps1",
                     vec!["-NoProfile".into(), "-NonInteractive".into(), "-File".into(), script.to_string_lossy().into()])
                }
            } else {
                let script = tmp.path().join("script.sh");
                let content = format!("#!/bin/bash\nset -e\n{}", code);
                std::fs::write(&script, &content)?;
                (bash().to_string(), content, ".sh", vec![script.to_string_lossy().into()])
            };
            let _ = (script_content, ext);
            let child = spawn_no_window(Command::new(&cmd).args(&args)
                .current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()))?;
            Ok(SpawnResult { child, _tmpdir: Some(tmp), compile_phase: None })
        }
        "browser" => {
            let pw = playwriter();
            let (bin, prefix): (&str, Vec<String>) = if pw.ends_with(".js") {
                ("node", vec![pw.to_string()])
            } else {
                (pw, vec![])
            };
            let trimmed = code.trim();
            let mut args = prefix;
            if trimmed.starts_with("playwriter ") {
                let rest = trimmed.strip_prefix("playwriter ").unwrap();
                args.extend(split_playwriter_args(rest));
            } else if trimmed.starts_with("session ") || trimmed == "session" {
                args.extend(shlex::split(trimmed).unwrap_or_else(|| trimmed.split_whitespace().map(str::to_string).collect()));
            } else {
                let pw_session = get_or_create_browser_session(bin, &args, cwd, session_id)
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                args.extend(["-s".into(), pw_session, "--timeout".into(), "14000".into(), "-e".into(), code.to_string()]);
            };
            let child = spawn_no_window(Command::new(bin)
                .args(&args)
                .current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()))?;
            Ok(SpawnResult { child, _tmpdir: None, compile_phase: None })
        }
        "deno" => {
            let tmp = tempfile::tempdir()?;
            let file = tmp.path().join("code.ts");
            std::fs::write(&file, code)?;
            let child = spawn_no_window(Command::new(deno())
                .args(["run", "--no-check", &file.to_string_lossy()])
                .current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()))?;
            Ok(SpawnResult { child, _tmpdir: Some(tmp), compile_phase: None })
        }
        "go" => {
            let tmp = tempfile::tempdir()?;
            let file = tmp.path().join("code.go");
            std::fs::write(&file, code)?;
            let child = spawn_no_window(Command::new(go())
                .args(["run", &file.to_string_lossy()])
                .current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()))?;
            Ok(SpawnResult { child, _tmpdir: Some(tmp), compile_phase: None })
        }
        "rust" => {
            let cargo_deps: Vec<(String, String)> = code.lines()
                .filter_map(|l| l.trim().strip_prefix("// cargo-dep:"))
                .filter_map(|dep| {
                    let parts: Vec<&str> = dep.splitn(2, '=').collect();
                    if parts.len() == 2 {
                        Some((parts[0].trim().to_string(), parts[1].trim().trim_matches('"').to_string()))
                    } else {
                        None
                    }
                })
                .collect();
            let cargo_paths: Vec<(String, String)> = code.lines()
                .filter_map(|l| l.trim().strip_prefix("// cargo-path:"))
                .filter_map(|dep| {
                    let parts: Vec<&str> = dep.splitn(2, '=').collect();
                    if parts.len() == 2 {
                        Some((parts[0].trim().to_string(), parts[1].trim().trim_matches('"').to_string()))
                    } else {
                        None
                    }
                })
                .collect();
            if cargo_deps.is_empty() && cargo_paths.is_empty() {
                let tmp = tempfile::tempdir()?;
                let file = tmp.path().join("code.rs");
                std::fs::write(&file, code)?;
                let bin_ext = if cfg!(windows) { ".exe" } else { "" };
                let bin_path = tmp.path().join(format!("code{}", bin_ext));
                let child = spawn_no_window(Command::new(rustc())
                    .args([file.to_string_lossy().as_ref(), "-o", bin_path.to_string_lossy().as_ref()])
                    .current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()))?;
                let phase = CompilePhase { bin_path, runtime: "rust".to_string(), cp: None, class_name: None, cwd: cwd.to_string(), _tmpdir: tmp };
                return Ok(SpawnResult { child, _tmpdir: None, compile_phase: Some(phase) });
            }
            let tmp = tempfile::tempdir()?;
            let src_dir = tmp.path().join("src");
            std::fs::create_dir_all(&src_dir)?;
            std::fs::write(src_dir.join("main.rs"), code)?;
            let mut dep_lines = String::new();
            for (name, version) in &cargo_deps {
                dep_lines.push_str(&format!("{} = \"{}\"\n", name, version));
            }
            for (name, path) in &cargo_paths {
                dep_lines.push_str(&format!("{} = {{ path = \"{}\" }}\n", name, path));
            }
            let cargo_toml = format!(
                "[package]\nname = \"exec-rust\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n{}",
                dep_lines
            );
            std::fs::write(tmp.path().join("Cargo.toml"), &cargo_toml)?;
            let child = spawn_no_window(Command::new("cargo")
                .args(["run", "--manifest-path", &tmp.path().join("Cargo.toml").to_string_lossy()])
                .current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()))?;
            Ok(SpawnResult { child, _tmpdir: Some(tmp), compile_phase: None })
        }
        "c" | "cpp" => {
            let tmp = tempfile::tempdir()?;
            let ext = if runtime == "c" { ".c" } else { ".cpp" };
            let file = tmp.path().join(format!("code{}", ext));
            std::fs::write(&file, code)?;
            let bin_ext = if cfg!(windows) { ".exe" } else { "" };
            let bin_path = tmp.path().join(format!("code{}", bin_ext));
            let compiler = if runtime == "c" { gcc() } else { gpp() };
            let child = spawn_no_window(Command::new(compiler)
                .args([file.to_string_lossy().as_ref(), "-o", bin_path.to_string_lossy().as_ref(), "-I", cwd])
                .current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()))?;
            let phase = CompilePhase { bin_path, runtime: runtime.to_string(), cp: None, class_name: None, cwd: cwd.to_string(), _tmpdir: tmp };
            Ok(SpawnResult { child, _tmpdir: None, compile_phase: Some(phase) })
        }
        "java" => {
            let tmp = tempfile::tempdir()?;
            let class_name = "Main";
            let file = tmp.path().join(format!("{}.java", class_name));
            let wrapped = format!(
                "public class {} {{\n  public static void main(String[] args) {{\n{}\n  }}\n}}",
                class_name,
                code.lines().map(|l| format!("    {}", l)).collect::<Vec<_>>().join("\n")
            );
            std::fs::write(&file, &wrapped)?;
            let sep = if cfg!(windows) { ";" } else { ":" };
            let cp = format!("{}{}{}", tmp.path().to_string_lossy(), sep, cwd);
            let child = spawn_no_window(Command::new(javac())
                .args(["-cp", &cp, &file.to_string_lossy()])
                .current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()))?;
            let phase = CompilePhase { bin_path: PathBuf::new(), runtime: "java".to_string(), cp: Some(cp), class_name: Some(class_name.to_string()), cwd: cwd.to_string(), _tmpdir: tmp };
            Ok(SpawnResult { child, _tmpdir: None, compile_phase: Some(phase) })
        }
        "serial" => {
            let mut parts = code.trim().split_whitespace();
            let port_name = parts.next().unwrap_or("COM1");
            let baud: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(115200);
            let tmp = tempfile::tempdir()?;
            let script_path = tmp.path().join("serial.mjs");
            let script = format!(
"import {{ SerialPort }} from 'serialport';\n\
const port = new SerialPort({{ path: '{}', baudRate: {} }});\n\
port.on('open', () => process.stderr.write('[serial] connected to {} at {}\\n'));\n\
port.on('data', buf => process.stdout.write(buf));\n\
port.on('error', e => {{ process.stderr.write('[serial error] ' + e.message + '\\n'); process.exit(1); }});\n\
port.on('close', () => {{ process.stderr.write('[serial] disconnected\\n'); process.exit(0); }});\n\
process.stdin.resume();\n\
process.stdin.on('data', buf => port.write(buf));\n",
                port_name, baud, port_name, baud);
            std::fs::write(&script_path, &script)?;
            let global_modules = std::process::Command::new("npm")
                .args(["root", "-g"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            let child = spawn_no_window(Command::new("node")
                .arg(&script_path)
                .env("NODE_PATH", &global_modules)
                .current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()))?;
            Ok(SpawnResult { child, _tmpdir: Some(tmp), compile_phase: None })
        }
        _ => Err(anyhow::anyhow!("Unsupported runtime: {}", runtime))
    }
}

fn find_free_port(start: u16) -> u16 {
    for port in start..start + 100 {
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
    start
}

fn browser_session_map_file() -> std::path::PathBuf {
    std::env::temp_dir().join("plugkit-browser-sessions.json")
}

fn register_browser_session(claude_session_id: &str, pw_session_id: &str) {
    if claude_session_id.is_empty() || pw_session_id.is_empty() { return; }
    let path = browser_session_map_file();
    let mut map: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let entry = map.entry(claude_session_id.to_string()).or_insert_with(|| serde_json::Value::Array(vec![]));
    if let serde_json::Value::Array(arr) = entry {
        let val = serde_json::Value::String(pw_session_id.to_string());
        if !arr.contains(&val) { arr.push(val); }
    }
    let _ = std::fs::write(&path, serde_json::to_string(&map).unwrap_or_default());
}

fn get_registered_sessions(claude_session_id: &str) -> Vec<String> {
    let path = browser_session_map_file();
    std::fs::read_to_string(&path).ok()
        .and_then(|s| serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&s).ok())
        .and_then(|m| m.get(claude_session_id).and_then(|v| v.as_array()).map(|arr| {
            arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
        }))
        .unwrap_or_default()
}

fn managed_browser_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().to_string());
    PathBuf::from(base).join("plugkit").join("chrome-portable")
}

fn system_chrome_exe() -> Option<PathBuf> {
    let candidates: Vec<PathBuf> = {
        let mut v = vec![];
        for var in &["PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA"] {
            if let Ok(base) = std::env::var(var) {
                v.push(PathBuf::from(&base).join("Google").join("Chrome").join("Application").join("chrome.exe"));
                v.push(PathBuf::from(&base).join("Chromium").join("Application").join("chrome.exe"));
            }
        }
        v.push(PathBuf::from("/usr/bin/google-chrome"));
        v.push(PathBuf::from("/usr/bin/chromium-browser"));
        v.push(PathBuf::from("/usr/bin/chromium"));
        v
    };
    candidates.into_iter().find(|p| p.exists())
        .or_else(|| which::which("google-chrome").ok())
        .or_else(|| which::which("chromium").ok())
        .or_else(|| which::which("chromium-browser").ok())
}

fn managed_browser_exe() -> Option<PathBuf> {
    let dir = managed_browser_dir();
    let portable_candidates = [
        dir.join("GoogleChromePortable").join("App").join("Chrome-bin").join("chrome.exe"),
        dir.join("GoogleChromePortable").join("App").join("Chrome").join("chrome"),
        dir.join("chrome").join("chrome"),
        dir.join("chrome"),
    ];
    portable_candidates.into_iter().find(|p| p.exists())
        .or_else(system_chrome_exe)
}

fn managed_browser_user_data(cwd: &str) -> PathBuf {
    acquire_profile_dir(cwd)
}

static PROFILE_LOCKS: std::sync::OnceLock<std::sync::Mutex<Vec<std::fs::File>>> = std::sync::OnceLock::new();
static PROFILE_CHOICE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, PathBuf>>> = std::sync::OnceLock::new();

fn acquire_profile_dir(cwd: &str) -> PathBuf {
    let choices = PROFILE_CHOICE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    {
        let guard = choices.lock().unwrap();
        if let Some(p) = guard.get(cwd) { return p.clone(); }
    }
    use fs2::FileExt;
    let shared = PathBuf::from(cwd).join(".plugkit-browser-profile");
    let _ = std::fs::create_dir_all(&shared);
    let shared_lock_path = shared.join(".rs-exec.lock");
    let chosen = match std::fs::OpenOptions::new().create(true).write(true).truncate(false).open(&shared_lock_path) {
        Ok(f) => {
            match f.try_lock_exclusive() {
                Ok(()) => {
                    PROFILE_LOCKS.get_or_init(|| std::sync::Mutex::new(Vec::new())).lock().unwrap().push(f);
                    shared
                }
                Err(_) => {
                    let pid = std::process::id();
                    let per_agent = PathBuf::from(cwd).join(format!(".plugkit-browser-profile-{}", pid));
                    let _ = std::fs::create_dir_all(&per_agent);
                    eprintln!("[browser] Shared profile in use by another runner; isolating this agent to {}.", per_agent.display());
                    if let Ok(f2) = std::fs::OpenOptions::new().create(true).write(true).truncate(false).open(per_agent.join(".rs-exec.lock")) {
                        let _ = f2.try_lock_exclusive();
                        PROFILE_LOCKS.get_or_init(|| std::sync::Mutex::new(Vec::new())).lock().unwrap().push(f2);
                    }
                    ensure_gitignore_entry(cwd, &format!(".plugkit-browser-profile-{}", pid));
                    per_agent
                }
            }
        }
        Err(_) => shared,
    };
    choices.lock().unwrap().insert(cwd.to_string(), chosen.clone());
    chosen
}

fn ensure_gitignore_entry(cwd: &str, entry: &str) {
    ensure_gitignore_entries(cwd, &[entry]);
}

static GITIGNORE_CHECKED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = std::sync::OnceLock::new();

fn ensure_gitignore_entries(cwd: &str, entries: &[&str]) {
    let cache = GITIGNORE_CHECKED.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()));
    { let g = cache.lock().unwrap(); if g.contains(cwd) { return; } }
    let gi = PathBuf::from(cwd).join(".gitignore");
    if !PathBuf::from(cwd).join(".git").exists() { cache.lock().unwrap().insert(cwd.to_string()); return; }
    let content = std::fs::read_to_string(&gi).unwrap_or_default();
    let existing: std::collections::HashSet<&str> = content.lines().map(|l| l.trim()).collect();
    let missing: Vec<&str> = entries.iter().copied().filter(|e| !existing.contains(*e)).collect();
    if !missing.is_empty() {
        let mut updated = content.clone();
        if !updated.is_empty() && !updated.ends_with('\n') { updated.push('\n'); }
        for e in missing { updated.push_str(e); updated.push('\n'); }
        let _ = std::fs::write(&gi, updated);
    }
    cache.lock().unwrap().insert(cwd.to_string());
}

pub fn ensure_plugkit_gitignore(cwd: &str) {
    ensure_gitignore_entries(cwd, PLUGKIT_IGNORE_PATTERNS);
}

const PLUGKIT_IGNORE_PATTERNS: &[&str] = &[
    ".plugkit-browser-profile/",
    ".plugkit-browser-profile-*/",
    ".plugkit-agent-worktree/",
    ".code-search/",
    ".codeinsight",
    "*.stackdump",
];

fn browser_port_map_file() -> std::path::PathBuf {
    std::env::temp_dir().join("plugkit-browser-ports.json")
}

fn get_session_browser_port(claude_session_id: &str) -> Option<u16> {
    let path = browser_port_map_file();
    std::fs::read_to_string(&path).ok()
        .and_then(|s| serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&s).ok())
        .and_then(|m| m.get(claude_session_id).and_then(|v| v.as_u64()).map(|p| p as u16))
}

fn set_session_browser_port(claude_session_id: &str, port: u16) {
    if claude_session_id.is_empty() { return; }
    let path = browser_port_map_file();
    let mut map: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    map.remove("");
    map.insert(claude_session_id.to_string(), serde_json::Value::Number(serde_json::Number::from(port)));
    let _ = std::fs::write(&path, serde_json::to_string(&map).unwrap_or_default());
}

fn remove_session_browser_port(claude_session_id: &str) {
    let path = browser_port_map_file();
    if let Ok(s) = std::fs::read_to_string(&path) {
        if let Ok(mut map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&s) {
            map.remove(claude_session_id);
            let _ = std::fs::write(&path, serde_json::to_string(&map).unwrap_or_default());
        }
    }
}

pub fn kill_session_browser(claude_session_id: &str) {
    if let Some(port) = get_session_browser_port(claude_session_id) {
        let mut sys = sysinfo::System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, false);
        let port_arg = format!("--remote-debugging-port={}", port);
        let roots: Vec<u32> = sys.processes().iter()
            .filter(|(_, proc)| {
                let cmd: Vec<String> = proc.cmd().iter().map(|s| s.to_string_lossy().to_lowercase()).collect();
                cmd.iter().any(|a| a.contains(port_arg.as_str()))
            })
            .map(|(pid, _)| pid.as_u32())
            .collect();
        let killed_any = !roots.is_empty();
        for pid in roots {
            eprintln!("[browser] Killing idle session browser pid tree {} on port {} for session {}.", pid, port, claude_session_id);
            crate::kill::kill_tree(pid);
        }
        if killed_any { kill_playwriter_ws_server(); }
        let port_path = browser_port_map_file();
        if let Ok(s) = std::fs::read_to_string(&port_path) {
            if let Ok(mut map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&s) {
                map.remove(claude_session_id);
                let _ = std::fs::write(&port_path, serde_json::to_string(&map).unwrap_or_default());
            }
        }
        let sess_path = browser_session_map_file();
        if let Ok(s) = std::fs::read_to_string(&sess_path) {
            if let Ok(mut map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&s) {
                map.remove(claude_session_id);
                let _ = std::fs::write(&sess_path, serde_json::to_string(&map).unwrap_or_default());
            }
        }
    }
}

fn ensure_managed_browser() -> Result<PathBuf, String> {
    if let Some(exe) = managed_browser_exe() {
        return Ok(exe);
    }
    let install_dir = managed_browser_dir();
    if install_dir.exists() && install_dir.read_dir().map(|mut d| d.next().is_none()).unwrap_or(false) {
        eprintln!("[browser] chrome-portable dir exists but is empty, removing for fresh install.");
        let _ = std::fs::remove_dir_all(&install_dir);
    }
    eprintln!("[browser] Managed browser not found. Installing Chrome Portable...");
    std::fs::create_dir_all(&install_dir)
        .map_err(|e| format!("Failed to create install dir: {}", e))?;

    let installer_url = "https://sourceforge.net/projects/portableapps/files/Google%20Chrome%20Portable/GoogleChromePortable_latest.paf.exe/download";
    let installer_path = install_dir.join("GoogleChromePortable_installer.exe");

    eprintln!("[browser] Downloading Chrome Portable installer...");
    let dl_result = Command::new("powershell")
        .args([
            "-NoProfile", "-NonInteractive", "-Command",
            &format!(
                "Invoke-WebRequest -Uri '{}' -OutFile '{}' -UseBasicParsing",
                installer_url,
                installer_path.display()
            ),
        ])
        .stdout(Stdio::piped()).stderr(Stdio::piped()).output();

    match dl_result {
        Ok(out) if out.status.success() => {
            eprintln!("[browser] Download complete. Running silent installer...");
        }
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(format!("Chrome Portable download failed: {}", err));
        }
        Err(e) => return Err(format!("Failed to run downloader: {}", e)),
    }

    let install_result = Command::new(&installer_path)
        .args([
            &format!("/DESTINATION={}", install_dir.display()),
            "/SILENT",
        ])
        .stdout(Stdio::piped()).stderr(Stdio::piped()).output();

    match install_result {
        Ok(out) if out.status.success() => {
            eprintln!("[browser] Chrome Portable installed.");
        }
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(format!("Chrome Portable installer failed: {}", err));
        }
        Err(e) => return Err(format!("Failed to run installer: {}", e)),
    }

    managed_browser_exe().ok_or_else(|| format!(
        "Chrome Portable installed but executable not found in {}. Check install logs.",
        install_dir.display()
    ))
}

fn kill_stale_managed_browser(user_data: &std::path::Path) {
    let profile_str = user_data.to_string_lossy().to_lowercase();
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, false);
    let roots: Vec<u32> = sys.processes().iter()
        .filter(|(_, proc)| {
            let cmd: Vec<String> = proc.cmd().iter().map(|s| s.to_string_lossy().to_lowercase()).collect();
            cmd.iter().any(|a| a.contains("user-data-dir") && a.contains(profile_str.as_str()))
        })
        .map(|(pid, _)| pid.as_u32())
        .collect();
    let killed_any = !roots.is_empty();
    for pid in roots {
        eprintln!("[browser] Killing stale managed browser pid tree {}.", pid);
        crate::kill::kill_tree(pid);
    }
    if killed_any { kill_playwriter_ws_server(); }
    std::thread::sleep(std::time::Duration::from_millis(500));
}

fn sanitize_chrome_exit_state(user_data: &std::path::Path) {
    let default_dir = user_data.join("Default");
    for stale in &["Last Session", "Last Tabs", "Current Session", "Current Tabs"] {
        let _ = std::fs::remove_file(default_dir.join(stale));
    }
    let prefs_path = default_dir.join("Preferences");
    if let Ok(content) = std::fs::read_to_string(&prefs_path) {
        if let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(obj) = json.as_object_mut() {
                let profile = obj.entry("profile").or_insert_with(|| serde_json::json!({}));
                if let Some(profile_obj) = profile.as_object_mut() {
                    profile_obj.insert("exit_type".into(), serde_json::json!("Normal"));
                    profile_obj.insert("exited_cleanly".into(), serde_json::json!(true));
                }
            }
            if let Ok(out) = serde_json::to_string(&json) {
                let _ = std::fs::write(&prefs_path, out);
            }
        }
    }
    let local_state_path = user_data.join("Local State");
    if let Ok(content) = std::fs::read_to_string(&local_state_path) {
        if let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(obj) = json.as_object_mut() {
                let user_exp = obj.entry("user_experience_metrics").or_insert_with(|| serde_json::json!({}));
                if let Some(ue) = user_exp.as_object_mut() {
                    let stab = ue.entry("stability").or_insert_with(|| serde_json::json!({}));
                    if let Some(stab_obj) = stab.as_object_mut() {
                        stab_obj.insert("exited_cleanly".into(), serde_json::json!(true));
                    }
                }
            }
            if let Ok(out) = serde_json::to_string(&json) {
                let _ = std::fs::write(&local_state_path, out);
            }
        }
    }
}

fn kill_playwriter_ws_server() {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, false);
    let targets: Vec<u32> = sys.processes().iter()
        .filter(|(_, proc)| {
            let name = proc.name().to_string_lossy().to_lowercase();
            let cmd = proc.cmd().iter().map(|s| s.to_string_lossy()).collect::<Vec<_>>().join(" ");
            name.contains("playwriter-ws-server") || cmd.contains("start-relay-server.js") || cmd.contains("start-relay-server.ts")
        })
        .map(|(pid, _)| pid.as_u32())
        .collect();
    for pid in targets {
        eprintln!("[browser] Killing stale playwriter-ws-server pid {} (will auto-restart on next use).", pid);
        crate::kill::kill_tree(pid);
    }
}

fn find_playwriter_extension() -> Option<std::path::PathBuf> {
    let candidates: Vec<std::path::PathBuf> = {
        let mut v = vec![];
        if let Ok(appdata) = std::env::var("APPDATA") {
            v.push(std::path::PathBuf::from(&appdata).join("npm").join("node_modules").join("playwriter").join("dist").join("extension"));
        }
        if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
            v.push(std::path::PathBuf::from(&home).join(".npm-global").join("lib").join("node_modules").join("playwriter").join("dist").join("extension"));
            v.push(std::path::PathBuf::from(&home).join(".local").join("lib").join("node_modules").join("playwriter").join("dist").join("extension"));
        }
        if let Ok(out) = Command::new("npm").args(["root", "-g"]).output() {
            let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !root.is_empty() {
                v.push(std::path::PathBuf::from(root).join("playwriter").join("dist").join("extension"));
            }
        }
        v
    };
    candidates.into_iter().find(|p| p.join("manifest.json").exists())
}

fn launch_managed_browser(exe: &PathBuf, port: u16, cwd: &str) -> Result<(), String> {
    let user_data = managed_browser_user_data(cwd);
    ensure_gitignore_entries(cwd, PLUGKIT_IGNORE_PATTERNS);
    kill_stale_managed_browser(&user_data);
    std::thread::sleep(std::time::Duration::from_millis(500));
    for lock_name in &["lockfile", "SingletonLock", "SingletonSocket", "SingletonCookie"] {
        let _ = std::fs::remove_file(user_data.join(lock_name));
    }
    std::fs::create_dir_all(&user_data)
        .map_err(|e| format!("Failed to create browser profile dir: {}", e))?;
    sanitize_chrome_exit_state(&user_data);
    eprintln!("[browser] Launching managed browser on port {}...", port);
    let mut args: Vec<String> = vec![
        format!("--remote-debugging-port={}", port),
        format!("--user-data-dir={}", user_data.display()),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--no-sandbox".into(),
        "--hide-crash-restore-bubble".into(),
        "--disable-session-crashed-bubble".into(),
        "--disable-features=InfiniteSessionRestore".into(),
        "--new-window".into(),
        "about:blank".into(),
    ];
    if let Some(ext_path) = find_playwriter_extension() {
        eprintln!("[browser] Loading playwriter extension from {}.", ext_path.display());
        args.push(format!("--load-extension={}", ext_path.display()));
    } else {
        eprintln!("[browser] Playwriter extension not found, launching without extension.");
    }
    let mut cmd = Command::new(exe);
    cmd.args(&args);
    cmd.stdout(Stdio::null()).stderr(Stdio::piped());
    #[cfg(windows)]
    { cmd.creation_flags(0x08000000); }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
    }
    let child = cmd.spawn().map_err(|e| format!("Failed to launch browser: {}", e))?;
    let pid = child.id();
    eprintln!("[browser] Spawned browser pid {}, verifying port {} responds...", pid, port);
    for i in 0..10 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            std::time::Duration::from_millis(200),
        ).is_ok() {
            eprintln!("[browser] Port {} responding after {}ms.", port, (i + 1) * 500);
            return Ok(());
        }
    }
    eprintln!("[browser] WARNING: Port {} not responding after 5s, proceeding anyway (retry loop will handle it).", port);
    Ok(())
}

fn try_new_session(bin: &str, prefix: &[String], cwd: &str, direct_arg: Option<&str>) -> Option<String> {
    let mut args: Vec<String> = prefix.to_vec();
    args.extend(["session".into(), "new".into()]);
    match direct_arg {
        Some(d) => args.push(d.to_string()),
        None => args.push("--direct".into()),
    }
    if let Ok(out) = Command::new(bin).args(&args).current_dir(cwd)
        .stdout(Stdio::piped()).stderr(Stdio::piped()).output()
    {
        let s = String::from_utf8_lossy(&out.stdout);
        for line in s.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() { continue; }
            if trimmed.chars().all(|c| c.is_ascii_digit()) {
                return Some(trimmed.to_string());
            }
            if let Some(rest) = trimmed.strip_prefix("Session ") {
                if let Some(id) = rest.split_whitespace().next() {
                    if id.chars().all(|c| c.is_ascii_digit()) {
                        return Some(id.to_string());
                    }
                }
            }
        }
    }
    None
}

fn profile_active_port(user_data: &std::path::Path) -> Option<u16> {
    let profile_str = user_data.to_string_lossy().to_lowercase();
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, false);
    for (_pid, proc) in sys.processes() {
        let cmd: Vec<String> = proc.cmd().iter().map(|s| s.to_string_lossy().to_lowercase()).collect();
        if cmd.iter().any(|a| a.contains("user-data-dir") && a.contains(profile_str.as_str())) {
            for arg in &cmd {
                if let Some(rest) = arg.strip_prefix("--remote-debugging-port=") {
                    if let Ok(p) = rest.parse::<u16>() {
                        return Some(p);
                    }
                }
            }
            return Some(0);
        }
    }
    None
}

fn port_belongs_to_session(port: u16, expected_profile: &std::path::Path) -> bool {
    let profile_str = expected_profile.to_string_lossy().to_lowercase();
    let port_arg = format!("--remote-debugging-port={}", port);
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, false);
    for (_pid, proc) in sys.processes() {
        let cmd: Vec<String> = proc.cmd().iter().map(|s| s.to_string_lossy().to_lowercase()).collect();
        if cmd.iter().any(|a| a.contains(&port_arg)) {
            let has_profile = cmd.iter().any(|a| a.contains(&profile_str));
            eprintln!("[browser] Port {} Chrome profile match: {} (expected: {})", port, has_profile, expected_profile.display());
            return has_profile;
        }
    }
    eprintln!("[browser] No Chrome process found on port {}.", port);
    false
}

fn get_or_create_browser_session(bin: &str, prefix: &[String], cwd: &str, claude_session_id: &str) -> Result<String, String> {
    eprintln!("[browser] Checking for existing owned session...");
    let owned_sessions = get_registered_sessions(claude_session_id);
    if !owned_sessions.is_empty() {
        let mut list_args: Vec<String> = prefix.to_vec();
        list_args.extend(["session".into(), "list".into()]);
        if let Ok(out) = Command::new(bin).args(&list_args).current_dir(cwd)
            .stdout(Stdio::piped()).stderr(Stdio::piped()).output()
        {
            let list = String::from_utf8_lossy(&out.stdout);
            let live_ids: Vec<String> = list.lines()
                .filter_map(|line| line.trim().split_whitespace().next().map(|s| s.to_string()))
                .filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()))
                .collect();
            for id in &owned_sessions {
                if live_ids.contains(id) {
                    eprintln!("[browser] Reusing owned session {}.", id);
                    return Ok(id.clone());
                }
            }
        }
    }

    eprintln!("[browser] No live owned session. Launching managed browser for this session...");
    let exe = ensure_managed_browser()?;
    let expected_profile = managed_browser_user_data(cwd);
    let port = if let Some(p) = get_session_browser_port(claude_session_id) {
        if port_belongs_to_session(p, &expected_profile) {
            let direct_arg = format!("--direct=localhost:{}", p);
            if let Some(id) = try_new_session(bin, prefix, cwd, Some(&direct_arg)) {
                eprintln!("[browser] Reconnected to existing session browser on port {}.", p);
                register_browser_session(claude_session_id, &id);
                return Ok(id);
            }
        }
        eprintln!("[browser] Existing session browser on port {} unreachable or belongs to another session, killing stale processes and launching new.", p);
        remove_session_browser_port(claude_session_id);
        kill_stale_managed_browser(&expected_profile);
        find_free_port(9222)
    } else {
        find_free_port(9222)
    };

    if expected_profile.exists() {
        if profile_active_port(&expected_profile).is_some() {
            eprintln!("[browser] Profile still active after kill attempt, force-killing...");
            kill_stale_managed_browser(&expected_profile);
        }
        for lock_name in &["lockfile", "SingletonLock", "SingletonSocket", "SingletonCookie"] {
            let _ = std::fs::remove_file(expected_profile.join(lock_name));
        }
    }

    let mut current_port = port;
    for outer in 0..3 {
        if outer > 0 {
            eprintln!("[browser] Outer retry {} — full reset (kill + fresh port + clean profile + relaunch).", outer);
            kill_stale_managed_browser(&expected_profile);
            kill_playwriter_ws_server();
            std::thread::sleep(std::time::Duration::from_millis(800));
            for lock_name in &["lockfile", "SingletonLock", "SingletonSocket", "SingletonCookie"] {
                let _ = std::fs::remove_file(expected_profile.join(lock_name));
            }
            current_port = find_free_port(current_port + 1);
        }
        launch_managed_browser(&exe, current_port, cwd)?;
        set_session_browser_port(claude_session_id, current_port);

        eprintln!("[browser] Waiting for managed browser on port {}...", current_port);
        let direct_arg = format!("--direct=localhost:{}", current_port);
        let mut got_session: Option<String> = None;
        for attempt in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(500));
            eprintln!("[browser] Retry {} — connecting to managed browser...", attempt + 1);
            if let Some(id) = try_new_session(bin, prefix, cwd, Some(&direct_arg)) {
                got_session = Some(id);
                break;
            }
            let port_alive = std::net::TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], current_port)),
                std::time::Duration::from_millis(200),
            ).is_ok();
            if !port_alive && attempt >= 4 {
                eprintln!("[browser] Port {} died mid-retry — breaking inner loop for outer reset.", current_port);
                break;
            }
        }
        if let Some(id) = got_session {
            eprintln!("[browser] Session {} created via managed browser on port {}.", id, current_port);
            register_browser_session(claude_session_id, &id);
            return Ok(id);
        }
        eprintln!("[browser] Outer attempt {} failed on port {}.", outer + 1, current_port);
    }

    let port_open = std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], current_port)),
        std::time::Duration::from_millis(500),
    ).is_ok();
    Err(format!(
        "Browser session creation failed after 3 outer attempts (60+ retries total) on port {}.\n\
         Port {} is {}. Chrome exe: {}\n\
         If port is closed, Chrome likely merged into an existing process or crashed.\n\
         Try: kill all Chrome processes and retry, or run with a clean profile.",
        current_port,
        current_port,
        if port_open { "OPEN (Chrome running but playwriter can't connect)" } else { "CLOSED (Chrome not listening — process likely exited)" },
        exe.display()
    ))
}

pub fn kill_child(child: &mut Child) {
    if cfg!(windows) {
        if let Some(pid) = child.id().checked_add(0) {
            let mut sys = sysinfo::System::new();
            sys.refresh_processes(sysinfo::ProcessesToUpdate::All, false);
            if let Some(proc) = sys.process(sysinfo::Pid::from_u32(pid)) {
                proc.kill();
            }
        }
    } else {
        let _ = child.kill();
    }
}
