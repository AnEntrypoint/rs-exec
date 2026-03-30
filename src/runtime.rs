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
fn playwriter() -> &'static str {
    PLAYWRITER.get_or_init(|| {
        if let Ok(p) = which::which("playwriter") {
            let dir = p.parent().unwrap_or(std::path::Path::new("."));
            let bin_js = dir.join("node_modules").join("playwriter").join("bin.js");
            if bin_js.exists() { return bin_js.to_string_lossy().to_string(); }
            return p.to_string_lossy().to_string();
        }
        "playwriter".to_string()
    })
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


pub fn spawn_process(runtime: &str, code: &str, cwd: &str) -> anyhow::Result<SpawnResult> {
    match runtime {
        "nodejs" | "typescript" => {
            let child = spawn_no_window(Command::new("bun")
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
            let (cmd, script_content, ext): (&str, String, &str) = if cfg!(windows) {
                (powershell(), format!("$ErrorActionPreference = 'Continue'\n{}", code), ".ps1")
            } else {
                (bash(), format!("#!/bin/bash\nset -e\n{}", code), ".sh")
            };
            let script = tmp.path().join(format!("script{}", ext));
            std::fs::write(&script, &script_content)?;
            let args: Vec<String> = if cfg!(windows) {
                vec!["-NoProfile".into(), "-NonInteractive".into(), "-File".into(), script.to_string_lossy().into()]
            } else {
                vec![script.to_string_lossy().into()]
            };
            let child = spawn_no_window(Command::new(cmd).args(&args)
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
                args.extend(trimmed.strip_prefix("playwriter ").unwrap().split_whitespace().map(|s| s.to_string()));
            } else if trimmed.starts_with("session ") || trimmed == "session" {
                args.extend(trimmed.split_whitespace().map(|s| s.to_string()));
            } else {
                let session_id = get_or_create_browser_session(bin, &args, cwd);
                args.extend(["-s".into(), session_id, "-e".into(), code.to_string()]);
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
        "rust" | "c" | "cpp" => {
            let tmp = tempfile::tempdir()?;
            let ext = match runtime { "rust" => ".rs", "c" => ".c", _ => ".cpp" };
            let file = tmp.path().join(format!("code{}", ext));
            std::fs::write(&file, code)?;
            let bin_ext = if cfg!(windows) { ".exe" } else { "" };
            let bin_path = tmp.path().join(format!("code{}", bin_ext));
            let compiler = match runtime { "rust" => rustc(), "c" => gcc(), _ => gpp() };
            let args: Vec<String> = match runtime {
                "rust" => vec![file.to_string_lossy().into(), "-o".into(), bin_path.to_string_lossy().into()],
                _ => vec![file.to_string_lossy().into(), "-o".into(), bin_path.to_string_lossy().into(), "-I".into(), cwd.to_string()],
            };
            let child = spawn_no_window(Command::new(compiler).args(&args)
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
        _ => Err(anyhow::anyhow!("Unsupported runtime: {}", runtime))
    }
}

fn get_or_create_browser_session(bin: &str, prefix: &[String], cwd: &str) -> String {
    let mut list_args: Vec<String> = prefix.to_vec();
    list_args.extend(["session".into(), "list".into()]);
    if let Ok(out) = Command::new(bin).args(&list_args).current_dir(cwd)
        .stdout(Stdio::piped()).stderr(Stdio::piped()).output()
    {
        let list = String::from_utf8_lossy(&out.stdout);
        for line in list.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                if let Some(id) = trimmed.split_whitespace().next() {
                    if id.chars().all(|c| c.is_ascii_digit()) {
                        return id.to_string();
                    }
                }
            }
        }
    }
    let mut new_args: Vec<String> = prefix.to_vec();
    new_args.extend(["session".into(), "new".into(), "--direct".into()]);
    if let Ok(out) = Command::new(bin).args(&new_args).current_dir(cwd)
        .stdout(Stdio::piped()).stderr(Stdio::piped()).output()
    {
        let s = String::from_utf8_lossy(&out.stdout);
        for line in s.lines() {
            let trimmed = line.trim();
            if trimmed.chars().all(|c| c.is_ascii_digit()) && !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    let launcher_js = r#"
const {getBrowserExecutableCandidates}=require('./dist/browser-config.js');
const {getBrowserLaunchArgs,getDefaultBrowserUserDataDir}=require('./dist/browser-launch.js');
const {spawn}=require('child_process');
const path=require('path');
const fs=require('fs');
const candidates=getBrowserExecutableCandidates();
const fallbacks=process.platform==='win32'?[process.env.ProgramFiles+'\\Google\\Chrome\\Application\\chrome.exe',process.env['ProgramFiles(x86)']+'\\Google\\Chrome\\Application\\chrome.exe',(process.env.LOCALAPPDATA||'')+'\\Google\\Chrome\\Application\\chrome.exe',process.env.ProgramFiles+'\\Microsoft\\Edge\\Application\\msedge.exe']:process.platform==='darwin'?['/Applications/Google Chrome.app/Contents/MacOS/Google Chrome']:['google-chrome','google-chrome-stable'].map(n=>{try{return require('child_process').execSync('which '+n,{encoding:'utf8'}).trim()}catch{return''}}).filter(Boolean);
const browserPath=[...candidates,...fallbacks].find(p=>p&&fs.existsSync(p));
if(!browserPath){process.stderr.write('No browser found');process.exit(1)}
const extPath=path.join(path.dirname(require.resolve('playwriter/package.json')),'dist','extension','chromium');
const userDataDir=getDefaultBrowserUserDataDir()+'-direct';
const args=getBrowserLaunchArgs({extensionPath:extPath,userDataDir,headless:false});
const net=require('net');
function findPort(start){return new Promise((res,rej)=>{const s=net.createServer();s.listen(start,'127.0.0.1',()=>{s.close(()=>res(start))});s.on('error',()=>start<9300?res(findPort(start+1)):rej(new Error('no free port')))})}
findPort(9222).then(port=>{
args.splice(args.length-1,0,'--remote-debugging-port='+port);
fs.mkdirSync(path.resolve(userDataDir),{recursive:true});
const p=spawn(browserPath,args,{detached:true,stdio:'ignore'});
p.unref();
process.stdout.write(port+'|'+String(p.pid||''));
});
"#;
    let pw_pkg = if bin == "node" && !prefix.is_empty() {
        std::path::Path::new(&prefix[0]).parent().and_then(|p| p.parent()).map(|p| p.to_path_buf())
    } else {
        which::which("playwriter").ok().and_then(|p| p.parent().map(|d| d.join("node_modules").join("playwriter")))
    };
    if let Some(ref pkg_dir) = pw_pkg {
        if pkg_dir.join("dist").join("browser-launch.js").exists() {
            let launcher_file = std::env::temp_dir().join("playwriter-launcher.cjs");
            let _ = std::fs::write(&launcher_file, launcher_js);
            if let Ok(out) = Command::new("node")
                .args(["-e", &format!("process.chdir({});{}",
                    serde_json::to_string(&pkg_dir.to_string_lossy().to_string()).unwrap_or_default(),
                    launcher_js)])
                .stdout(Stdio::piped()).stderr(Stdio::piped()).output()
            {
                let launch_out = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !launch_out.is_empty() {
                    let port = launch_out.split('|').next().unwrap_or("9222");
                    let direct_arg = format!("--direct=localhost:{}", port);
                    for _ in 0..20 {
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        let mut retry_args: Vec<String> = prefix.to_vec();
                        retry_args.extend(["session".into(), "new".into(), direct_arg.clone()]);
                        if let Ok(out) = Command::new(bin).args(&retry_args).current_dir(cwd)
                            .stdout(Stdio::piped()).stderr(Stdio::piped()).output()
                        {
                            let s = String::from_utf8_lossy(&out.stdout);
                            for line in s.lines() {
                                let trimmed = line.trim();
                                if trimmed.chars().all(|c| c.is_ascii_digit()) && !trimmed.is_empty() {
                                    let _ = std::fs::remove_file(&launcher_file);
                                    return trimmed.to_string();
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    "1".to_string()
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
