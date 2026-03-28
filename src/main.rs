mod daemon;
mod background_tasks;
mod runtime;
mod runner;
mod rpc_client;

use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::{env, fs, path::PathBuf, time::Duration};

const HARD_CEILING_MS: u64 = 15000;
const BM2_NAME: &str = "rs-exec-runner";

fn port_file() -> PathBuf {
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

#[derive(Subcommand)]
enum Cmd {
    Exec {
        #[arg(long)] lang: Option<String>,
        #[arg(long)] cwd: Option<String>,
        #[arg(long)] file: Option<String>,
        code: Vec<String>,
    },
    Bash {
        #[arg(long)] cwd: Option<String>,
        commands: Vec<String>,
    },
    Status { task_id: String },
    Sleep { task_id: String, seconds: Option<u64>, #[arg(long)] next_output: bool },
    Close { task_id: String },
    #[command(name = "type")] Type { task_id: String, input: Vec<String> },
    Runner { sub: String },
    Pm2list,
}

async fn ensure_runner() -> anyhow::Result<()> {
    if rpc_client::health_check().await { return Ok(()); }
    tokio::time::sleep(Duration::from_millis(1000)).await;
    if rpc_client::health_check().await { return Ok(()); }
    tokio::time::sleep(Duration::from_millis(2000)).await;
    if rpc_client::health_check().await { return Ok(()); }
    eprintln!("Auto-starting runner...");
    daemon::start(BM2_NAME, &self_exe(), &["--runner-mode"])?;
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if rpc_client::health_check().await { return Ok(()); }
    }
    Err(anyhow::anyhow!("Runner did not become healthy in time"))
}

fn parse_task_id(s: &str) -> u64 {
    s.trim_start_matches("task_").parse().unwrap_or(0)
}

async fn run_code(code: &str, runtime: &str, cwd: &str) -> anyhow::Result<i32> {
    ensure_runner().await?;
    let task_id_val = rpc_client::rpc_call("createTask", json!({ "code": code, "runtime": runtime, "workingDirectory": cwd }), 10000).await?;
    let task_id = task_id_val["taskId"].as_u64().unwrap_or(0);

    let safety = tokio::time::sleep(Duration::from_millis(HARD_CEILING_MS));
    tokio::pin!(safety);

    let exec_fut = rpc_client::rpc_call(
        "execute",
        json!({ "code": code, "runtime": runtime, "workingDirectory": cwd, "timeout": HARD_CEILING_MS, "backgroundTaskId": task_id }),
        HARD_CEILING_MS + 5000,
    );

    let result = tokio::select! {
        r = exec_fut => match r {
            Ok(v) => v["result"].clone(),
            Err(e) => json!({ "error": e.to_string() }),
        },
        _ = safety => json!({ "backgroundTaskId": task_id, "persisted": true }),
    };

    if result["persisted"].as_bool().unwrap_or(false) || (result["backgroundTaskId"].is_u64() && !result["completed"].as_bool().unwrap_or(false)) {
        let id = format!("task_{}", result["backgroundTaskId"].as_u64().unwrap_or(task_id));
        let partial = rpc_client::rpc_call("getAndClearOutput", json!({ "taskId": task_id }), 5000).await.ok();
        if let Some(out) = partial {
            if let Some(arr) = out["output"].as_array() {
                for entry in arr {
                    let d = entry["d"].as_str().unwrap_or("");
                    if entry["s"] == "stdout" { print!("{}", d); } else { eprint!("{}", d); }
                }
            }
        }
        println!("\nStill running after 15s — backgrounded.\nTask ID: {}\n", id);
        println!("  rs-exec sleep {}       # wait for completion (up to 30s)", id);
        println!("  rs-exec status {}      # drain output buffer", id);
        println!("  rs-exec close {}       # delete task when done", id);
        println!("  rs-exec runner stop    # stop runner when all tasks done");
        return Ok(0);
    }

    if result["backgroundTaskId"].is_u64() && result["completed"].as_bool().unwrap_or(false) {
        let _ = rpc_client::rpc_call("deleteTask", json!({ "taskId": result["backgroundTaskId"] }), 5000).await;
    } else {
        let _ = rpc_client::rpc_call("deleteTask", json!({ "taskId": task_id }), 5000).await;
    }

    if let Some(s) = result["stdout"].as_str() { if !s.is_empty() { print!("{}", s); } }
    if let Some(s) = result["stderr"].as_str() { if !s.is_empty() { eprint!("{}", s); } }
    if let Some(e) = result["error"].as_str() { if !e.is_empty() { eprintln!("Error: {}", e); return Ok(1); } }

    let exit_code = result["exitCode"].as_i64().unwrap_or(0) as i32;
    if result["success"].as_bool() == Some(false) { return Ok(if exit_code != 0 { exit_code } else { 1 }); }
    Ok(exit_code)
}

async fn cmd_status(task_id_str: &str) -> anyhow::Result<()> {
    ensure_runner().await?;
    let raw_id = parse_task_id(task_id_str);
    let task = rpc_client::rpc_call("getTask", json!({ "taskId": raw_id }), 10000).await?;
    let task = &task["task"];
    if task.is_null() { eprintln!("Task not found"); std::process::exit(1); }
    println!("Status: {}", task["status"].as_str().unwrap_or("unknown"));
    if let Some(r) = task["result"].as_object() {
        if let Some(s) = r.get("stdout").and_then(|v| v.as_str()) { if !s.is_empty() { print!("{}", s); } }
        if let Some(s) = r.get("stderr").and_then(|v| v.as_str()) { if !s.is_empty() { eprint!("{}", s); } }
    }
    let output = rpc_client::rpc_call("getAndClearOutput", json!({ "taskId": raw_id }), 5000).await?;
    if let Some(arr) = output["output"].as_array() {
        for e in arr { let d = e["d"].as_str().unwrap_or(""); if e["s"] == "stdout" { print!("{}", d); } else { eprint!("{}", d); } }
    }
    Ok(())
}

async fn cmd_sleep(task_id_str: &str, secs: u64, next_output: bool) -> anyhow::Result<()> {
    ensure_runner().await?;
    let raw_id = parse_task_id(task_id_str);
    let timeout = Duration::from_secs(secs);
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() >= timeout { break; }
        let task = rpc_client::rpc_call("getTask", json!({ "taskId": raw_id }), 5000).await?;
        let task = &task["task"];
        if task.is_null() { println!("Task not found or already completed."); return Ok(()); }
        let output = rpc_client::rpc_call("getAndClearOutput", json!({ "taskId": raw_id }), 5000).await?;
        if let Some(arr) = output["output"].as_array() {
            for e in arr { let d = e["d"].as_str().unwrap_or(""); if e["s"] == "stdout" { print!("{}", d); } else { eprint!("{}", d); } }
        }
        let status = task["status"].as_str().unwrap_or("");
        if status != "running" && status != "pending" {
            println!("\nTask finished ({}).\n  rs-exec close {}      # delete task", status, task_id_str);
            return Ok(());
        }
        if next_output {
            let remaining = timeout.saturating_sub(start.elapsed()).min(Duration::from_secs(30));
            let _ = rpc_client::rpc_call("waitForOutput", json!({ "taskId": raw_id, "timeoutMs": remaining.as_millis() as u64 }), remaining.as_millis() as u64 + 5000).await;
        } else {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    println!("\nTimeout after {}s. Task still running.\n  rs-exec sleep {}       # wait again", secs, task_id_str);
    Ok(())
}

async fn cmd_close(task_id_str: &str) -> anyhow::Result<()> {
    ensure_runner().await?;
    rpc_client::rpc_call("deleteTask", json!({ "taskId": parse_task_id(task_id_str) }), 10000).await?;
    println!("Task {} closed", task_id_str);
    Ok(())
}

async fn cmd_type(task_id_str: &str, input: &str) -> anyhow::Result<()> {
    ensure_runner().await?;
    let res = rpc_client::rpc_call("sendStdin", json!({ "taskId": parse_task_id(task_id_str), "data": format!("{}\n", input) }), 10000).await?;
    if res["ok"].as_bool().unwrap_or(false) { println!("Sent to task {}", task_id_str); }
    else { eprintln!("Task {} not found or not running", task_id_str); }
    Ok(())
}

fn print_running_tools() {
    let procs = daemon::list();
    let online: Vec<_> = procs.iter().filter(|p| p.status == "online").collect();
    if online.is_empty() { eprintln!("\n[Running tools: none]"); }
    else {
        eprintln!("\n[Running tools]");
        for p in &online { eprintln!("  {}  pid={}", p.name, p.pid.map(|p| p.to_string()).unwrap_or_else(|| "n/a".into())); }
        eprintln!("  Tip: rs-exec sleep <task_id>   # wait for a task");
    }
}

#[tokio::main]
async fn main() {
    if env::args().any(|a| a == "--runner-mode") {
        runner::run_server().await.expect("Runner failed");
        return;
    }

    let cli = Cli::parse();
    let mut exit_code = 0i32;

    let result: anyhow::Result<()> = async {
        match cli.command {
            Cmd::Exec { lang, cwd, file, code } => {
                let code_str = if let Some(f) = file { fs::read_to_string(f)? } else { code.join(" ") };
                if code_str.trim().is_empty() { eprintln!("No code provided"); exit_code = 1; return Ok(()); }
                let cwd = cwd.unwrap_or_else(|| env::current_dir().unwrap().to_string_lossy().to_string());
                let mut runtime = lang.unwrap_or_else(|| "nodejs".into());
                if runtime == "typescript" || runtime == "auto" { runtime = "nodejs".into(); }
                exit_code = run_code(&code_str, &runtime, &cwd).await?;
            }
            Cmd::Bash { cwd, commands } => {
                let cmd = commands.join(" ");
                if cmd.trim().is_empty() { eprintln!("No commands provided"); exit_code = 1; return Ok(()); }
                let cwd = cwd.unwrap_or_else(|| env::current_dir().unwrap().to_string_lossy().to_string());
                let runtime = if cfg!(windows) { "powershell" } else { "bash" };
                exit_code = run_code(&cmd, runtime, &cwd).await?;
            }
            Cmd::Status { task_id } => cmd_status(&task_id).await?,
            Cmd::Sleep { task_id, seconds, next_output } => cmd_sleep(&task_id, seconds.unwrap_or(30), next_output).await?,
            Cmd::Close { task_id } => cmd_close(&task_id).await?,
            Cmd::Type { task_id, input } => cmd_type(&task_id, &input.join(" ")).await?,
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
            Cmd::Pm2list => {
                ensure_runner().await?;
                let res = rpc_client::rpc_call("pm2list", json!({}), 10000).await?;
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
    print_running_tools();
    std::process::exit(exit_code);
}
