mod daemon;
mod background_tasks;
mod kill;
mod runtime;
mod rpc;
mod runner;
mod rpc_client;

use clap::{Parser, Subcommand};
use serde_json::json;
use std::{env, fs, path::PathBuf, time::Duration};

const BM2_NAME: &str = "rs-exec-runner";

fn port_file() -> PathBuf {
    if let Ok(p) = env::var("RS_EXEC_PORT_FILE") { return PathBuf::from(p); }
    env::temp_dir().join("glootie-runner.port")
}

fn self_exe() -> String {
    env::current_exe().unwrap_or_default().to_string_lossy().to_string()
}

#[derive(Parser)]
#[command(name = "rs-exec", about = "rs-exec — code execution CLI")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

fn parse_timeout_ms(s: &str) -> Result<u64, String> {
    let n: u64 = s.parse().map_err(|_| format!("invalid --timeout (ms): {}", s))?;
    if n == 0 { return Err("--timeout must be > 0 ms".into()); }
    Ok(n)
}

#[derive(Subcommand)]
enum Cmd {
    Exec {
        #[arg(long)] lang: Option<String>,
        #[arg(long)] cwd: Option<String>,
        #[arg(long)] file: Option<String>,
        #[arg(long, alias = "timeout-ms", value_parser = parse_timeout_ms, help = "Mandatory execution timeout in milliseconds (alias: --timeout-ms)")] timeout: u64,
        code: Vec<String>,
    },
    Bash {
        #[arg(long)] cwd: Option<String>,
        #[arg(long, alias = "timeout-ms", value_parser = parse_timeout_ms, help = "Mandatory execution timeout in milliseconds (alias: --timeout-ms)")] timeout: u64,
        commands: Vec<String>,
    },
    Runner { sub: String },
    Pm2list,
    #[command(name = "kill-port")] KillPort { port: u16 },
    #[command(name = "session-cleanup")] SessionCleanup { #[arg(long)] session: String },
}

async fn ensure_runner() -> anyhow::Result<()> {
    // Fast path: already healthy.
    if rpc_client::health_check().await { return Ok(()); }
    // No retry-with-backoff before spawning. health_check fails fast on stale
    // port file (connect refused) and bounded on wedged listener (read timeout).
    // Either way, the right answer is to spawn a fresh runner — it will
    // atomically replace port_file with its own OS-assigned port, after which
    // health_check resolves cleanly. Backoff before spawn just compounded the
    // hang when port_file pointed at a half-dead Windows orphan listener.
    eprintln!("Auto-starting runner...");
    daemon::start(BM2_NAME, &self_exe(), &["--runner-mode"])?;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if rpc_client::health_check().await { return Ok(()); }
    }
    Err(anyhow::anyhow!("Runner did not become healthy in time"))
}

fn normalize_code_input(raw: String) -> String {
    raw.trim_start_matches('\u{feff}').to_string()
}

async fn run_code(code: &str, runtime: &str, cwd: &str, timeout_ms: u64) -> anyhow::Result<i32> {
    ensure_runner().await?;
    let task_id_val = rpc_client::rpc_call("createTask", json!({ "code": code, "runtime": runtime, "workingDirectory": cwd }), 0).await?;
    let task_id = task_id_val["taskId"].as_u64().unwrap_or(0);

    let rpc_deadline_ms = timeout_ms.saturating_add(5_000);
    let result = match rpc_client::rpc_call(
        "execute",
        json!({ "code": code, "runtime": runtime, "workingDirectory": cwd, "backgroundTaskId": task_id, "timeoutMs": timeout_ms }),
        rpc_deadline_ms,
    ).await {
        Ok(v) => v["result"].clone(),
        Err(e) => json!({ "error": e.to_string() }),
    };

    let printed_from_output = if let Some(arr) = result["output"].as_array() {
        let mut printed = false;
        for e in arr {
            let d = e["d"].as_str().unwrap_or("");
            if e["s"] == "stdout" { print!("{}", d); } else { eprint!("{}", d); }
            if !d.is_empty() { printed = true; }
        }
        printed
    } else { false };
    if !printed_from_output {
        if let Some(s) = result["stdout"].as_str() { if !s.is_empty() { print!("{}", s); } }
        if let Some(s) = result["stderr"].as_str() { if !s.is_empty() { eprint!("{}", s); } }
    }
    if let Some(e) = result["error"].as_str() { if !e.is_empty() { eprintln!("Error: {}", e); return Ok(1); } }

    let exit_code = result["exitCode"].as_i64().unwrap_or(0) as i32;
    if result["success"].as_bool() == Some(false) { return Ok(if exit_code != 0 { exit_code } else { 1 }); }
    Ok(exit_code)
}

#[tokio::main]
async fn main() {
    if env::args().any(|a| a == "--exec-process-mode") {
        rs_exec::run_exec_process();
        return;
    }
    rs_exec::install_broken_pipe_handler();
    if env::args().any(|a| a == "--runner-mode") {
        runner::run_server().await.expect("Runner failed");
        return;
    }

    let cli = Cli::parse();
    let mut exit_code = 0i32;

    let result: anyhow::Result<()> = async {
        match cli.command {
            Cmd::Exec { lang, cwd, file, timeout, code } => {
                let code_str = if let Some(f) = file { normalize_code_input(fs::read_to_string(f)?) } else { normalize_code_input(code.join(" ")) };
                if code_str.trim().is_empty() { eprintln!("No code provided"); exit_code = 1; return Ok(()); }
                let cwd = cwd.unwrap_or_else(|| env::current_dir().unwrap().to_string_lossy().to_string());
                let mut runtime = lang.unwrap_or_else(|| "nodejs".into());
                if runtime == "typescript" || runtime == "auto" { runtime = "nodejs".into(); }
                exit_code = run_code(&code_str, &runtime, &cwd, timeout).await?;
            }
            Cmd::Bash { cwd, timeout, commands } => {
                let cmd = commands.join(" ");
                if cmd.trim().is_empty() { eprintln!("No commands provided"); exit_code = 1; return Ok(()); }
                let cwd = cwd.unwrap_or_else(|| env::current_dir().unwrap().to_string_lossy().to_string());
                let runtime = if cfg!(windows) { "cmd" } else { "bash" };
                exit_code = run_code(&cmd, runtime, &cwd, timeout).await?;
            }
            Cmd::Runner { sub } => match sub.as_str() {
                "start" => {
                    if rpc_client::health_check().await {
                        println!("Runner already healthy on port {}", fs::read_to_string(port_file()).unwrap_or_default().trim().to_string());
                        return Ok(());
                    }
                    daemon::start(BM2_NAME, &self_exe(), &["--runner-mode"])?;
                    for _ in 0..20 {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        if rpc_client::health_check().await {
                            println!("Runner started on port {}", fs::read_to_string(port_file()).unwrap_or_default().trim().to_string());
                            return Ok(());
                        }
                    }
                    return Err(anyhow::anyhow!("Runner did not become healthy"));
                }
                "stop" => { daemon::kill(BM2_NAME); println!("Runner stopped"); }
                "status" => {
                    match daemon::describe(BM2_NAME) {
                        None => println!("{}: not found", BM2_NAME),
                        Some(d) => {
                            println!("name:     {}", d.name);
                            println!("status:   {}", d.status);
                            println!("pid:      {}", d.pid.map(|p| p.to_string()).unwrap_or_else(|| "n/a".into()));
                            if let Ok(p) = fs::read_to_string(port_file()) { println!("port:     {}", p.trim()); }
                        }
                    }
                }
                _ => { eprintln!("Unknown runner subcommand: {}", sub); exit_code = 1; }
            }
            Cmd::KillPort { port } => {
                ensure_runner().await?;
                let res = rpc_client::rpc_call("killPort", json!({ "port": port }), 0).await?;
                if res["ok"].as_bool().unwrap_or(false) {
                    println!("Killed process on port {} (pid {})", port, res["killedPid"]);
                } else {
                    eprintln!("No process found listening on port {}", port);
                    exit_code = 1;
                }
            }
            Cmd::SessionCleanup { session } => {
                if session.is_empty() { return Ok(()); }
                ensure_runner().await?;
                rpc_client::rpc_call("deleteSessionTasks", json!({ "sessionId": session }), 0).await?;
                runtime::kill_session_browser(&session);
            }
            Cmd::Pm2list => {
                ensure_runner().await?;
                let res = rpc_client::rpc_call("pm2list", json!({}), 0).await?;
                let procs = daemon::list();
                let online: Vec<_> = procs.iter().filter(|p| p.status == "online").collect();
                if online.is_empty() && res["processes"].as_array().map(|a| a.is_empty()).unwrap_or(true) {
                    println!("No processes found.");
                } else {
                    for p in online { println!("{}  status={}  pid={}", p.name, p.status, p.pid.map(|p| p.to_string()).unwrap_or_else(|| "n/a".into())); }
                    if let Some(arr) = res["processes"].as_array() {
                        for p in arr { println!("{}  status={}  pid={}", p["name"].as_str().unwrap_or("?"), p["status"].as_str().unwrap_or("?"), p["pid"]); }
                    }
                }
            }
        }
        Ok(())
    }.await;

    if let Err(e) = result { eprintln!("Error: {}", e); exit_code = 1; }
    std::process::exit(exit_code);
}
