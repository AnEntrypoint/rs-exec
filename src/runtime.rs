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
        if which::which(b).is_ok() {
            return b.to_string();
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
