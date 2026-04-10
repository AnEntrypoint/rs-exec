pub fn kill_tree(pid: u32) {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, false);
    let root = sysinfo::Pid::from_u32(pid);
    let mut queue = vec![root];
    let mut desc: Vec<sysinfo::Pid> = Vec::new();
    while let Some(p) = queue.pop() {
        for (cp, proc) in sys.processes() { if proc.parent() == Some(p) { desc.push(*cp); queue.push(*cp); } }
    }
    for d in desc { if let Some(proc) = sys.process(d) { proc.kill(); } }
    if let Some(proc) = sys.process(root) { proc.kill(); }
}
