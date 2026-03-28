use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use sysinfo::{Pid, System};

fn daemon_dir() -> PathBuf {
    std::env::temp_dir().join("rs-exec-daemon")
}

fn pid_file(name: &str) -> PathBuf {
    daemon_dir().join(format!("{}.pid", name))
}

fn log_file(name: &str, stream: &str) -> PathBuf {
    daemon_dir().join(format!("{}-{}.log", name, stream))
}

fn is_alive(pid: u32) -> bool {
    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, false);
    sys.process(Pid::from_u32(pid)).is_some()
}

fn read_pid(name: &str) -> Option<u32> {
    let s = fs::read_to_string(pid_file(name)).ok()?;
    let pid: u32 = s.trim().parse().ok()?;
    if is_alive(pid) {
        Some(pid)
    } else {
        let _ = fs::remove_file(pid_file(name));
        None
    }
}

pub struct ProcInfo {
    pub name: String,
    pub pid: Option<u32>,
    pub status: &'static str,
}

pub fn start(name: &str, exe: &str, args: &[&str]) -> anyhow::Result<u32> {
    fs::create_dir_all(daemon_dir())?;
    if read_pid(name).is_some() {
        kill(name);
    }
    let out_f = fs::File::create(log_file(name, "out"))?;
    let err_f = fs::File::create(log_file(name, "err"))?;

    #[cfg(windows)]
    let child = {
        use std::os::windows::process::CommandExt;
        Command::new(exe)
            .args(args)
            .stdout(out_f)
            .stderr(err_f)
            .stdin(Stdio::null())
            .creation_flags(0x00000008)
            .spawn()?
    };
    #[cfg(not(windows))]
    let child = {
        Command::new(exe)
            .args(args)
            .stdout(out_f)
            .stderr(err_f)
            .stdin(Stdio::null())
            .spawn()?
    };

    let pid = child.id();
    fs::write(pid_file(name), pid.to_string())?;
    std::mem::forget(child);
    Ok(pid)
}

pub fn kill(name: &str) -> bool {
    let Some(pid) = read_pid(name) else { return false };
    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, false);
    if let Some(proc) = sys.process(Pid::from_u32(pid)) {
        proc.kill();
    }
    let _ = fs::remove_file(pid_file(name));
    true
}

pub fn list() -> Vec<ProcInfo> {
    let _ = fs::create_dir_all(daemon_dir());
    let Ok(entries) = fs::read_dir(daemon_dir()) else {
        return vec![];
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".pid"))
        .map(|e| {
            let fname = e.file_name();
            let name = fname.to_string_lossy().trim_end_matches(".pid").to_string();
            let pid = read_pid(&name);
            ProcInfo {
                status: if pid.is_some() { "online" } else { "stopped" },
                name,
                pid,
            }
        })
        .collect()
}

pub fn describe(name: &str) -> Option<ProcInfo> {
    let pid = read_pid(name)?;
    Some(ProcInfo {
        name: name.to_string(),
        pid: Some(pid),
        status: "online",
    })
}
