#![allow(dead_code)]
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ChildStdin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

#[derive(Debug, Clone, PartialEq)]
pub enum TaskStatus { Pending, Running, Completed, Failed }

#[derive(Debug, Clone)]
pub struct OutputEntry { pub stream: String, pub data: String }

#[derive(Debug, Clone)]
pub struct TaskResult { pub success: bool, pub stdout: String, pub stderr: String, pub error: Option<String>, pub exit_code: i32 }

#[derive(Debug, Clone)]
pub struct RunningTaskMeta { pub id: u64, pub cmd_summary: String, pub elapsed_ms: u128 }

pub fn task_log_path(session_id: &str, task_id: u64) -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let sid_dir = if session_id.is_empty() { "no-session".to_string() } else { session_id.to_string() };
    home.join(".claude").join("gm-log").join("tasks").join(sid_dir).join(format!("{}.log", task_id))
}

pub fn append_logfile(path: &Path, stream: &str, data: &str) {
    use std::io::Write;
    if let Some(parent) = path.parent() { let _ = std::fs::create_dir_all(parent); }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[{}] {}", stream, data.trim_end_matches('\n'));
    }
}

#[derive(Debug)]
pub struct Task {
    pub id: u64, pub status: TaskStatus, pub result: Option<TaskResult>,
    pub output_log: Vec<OutputEntry>, pub created_at: Instant,
    pub completed_at: Option<Instant>, pub session_id: Option<String>,
    pub log_path: Option<PathBuf>, pub cmd_summary: String,
}

pub struct BackgroundTaskStore {
    tasks: Mutex<HashMap<u64, Task>>,
    counter: Mutex<u64>,
    notifiers: Mutex<HashMap<u64, Arc<Notify>>>,
}

impl BackgroundTaskStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { tasks: Mutex::new(HashMap::new()), counter: Mutex::new(0), notifiers: Mutex::new(HashMap::new()) })
    }

    pub fn create_task(&self) -> u64 {
        let mut c = self.counter.lock().unwrap(); *c += 1; let id = *c;
        self.tasks.lock().unwrap().insert(id, Task { id, status: TaskStatus::Pending, result: None, output_log: vec![], created_at: Instant::now(), completed_at: None, session_id: None, log_path: None, cmd_summary: String::new() });
        id
    }

    pub fn set_log_path(&self, id: u64, p: PathBuf) {
        if let Some(t) = self.tasks.lock().unwrap().get_mut(&id) { t.log_path = Some(p); }
    }

    pub fn get_log_path(&self, id: u64) -> Option<PathBuf> {
        self.tasks.lock().unwrap().get(&id).and_then(|t| t.log_path.clone())
    }

    pub fn set_cmd_summary(&self, id: u64, s: &str) {
        if let Some(t) = self.tasks.lock().unwrap().get_mut(&id) {
            let trimmed: String = s.chars().take(60).collect();
            t.cmd_summary = trimmed;
        }
    }

    pub fn running_meta_for_session(&self, sid: &str) -> Vec<RunningTaskMeta> {
        let now = Instant::now();
        self.tasks.lock().unwrap().values()
            .filter(|t| t.session_id.as_deref() == Some(sid) && matches!(t.status, TaskStatus::Pending | TaskStatus::Running))
            .map(|t| RunningTaskMeta { id: t.id, cmd_summary: t.cmd_summary.clone(), elapsed_ms: now.saturating_duration_since(t.created_at).as_millis() })
            .collect()
    }

    pub fn set_session_id(&self, id: u64, session_id: &str) {
        if let Some(t) = self.tasks.lock().unwrap().get_mut(&id) { t.session_id = Some(session_id.to_string()); }
    }

    pub fn session_task_ids(&self, session_id: &str) -> Vec<u64> {
        self.tasks.lock().unwrap().values().filter(|t| t.session_id.as_deref() == Some(session_id)).map(|t| t.id).collect()
    }

    pub fn delete_session_tasks(&self, session_id: &str) -> u64 {
        let mut tasks = self.tasks.lock().unwrap();
        let ids: Vec<u64> = tasks.values().filter(|t| t.session_id.as_deref() == Some(session_id)).map(|t| t.id).collect();
        let count = ids.len() as u64;
        for id in &ids { tasks.remove(id); } drop(tasks);
        let mut notifiers = self.notifiers.lock().unwrap();
        for id in &ids { notifiers.remove(id); }
        count
    }

    pub fn start_task(&self, id: u64) {
        if let Some(t) = self.tasks.lock().unwrap().get_mut(&id) { t.status = TaskStatus::Running; }
    }

    pub fn complete_task(&self, id: u64, result: TaskResult) {
        { let mut tasks = self.tasks.lock().unwrap(); if let Some(t) = tasks.get_mut(&id) { t.status = TaskStatus::Completed; t.result = Some(result); t.completed_at = Some(Instant::now()); } }
        self.notify(id);
    }

    pub fn fail_task(&self, id: u64, error: String) {
        { let mut tasks = self.tasks.lock().unwrap(); if let Some(t) = tasks.get_mut(&id) { t.status = TaskStatus::Failed; t.result = Some(TaskResult { success: false, stdout: String::new(), stderr: String::new(), error: Some(error), exit_code: 1 }); t.completed_at = Some(Instant::now()); } }
        self.notify(id);
    }

    pub fn append_output(&self, id: u64, stream: &str, data: &str) {
        { let mut tasks = self.tasks.lock().unwrap(); if let Some(t) = tasks.get_mut(&id) { t.output_log.push(OutputEntry { stream: stream.to_string(), data: data.to_string() }); let total: usize = t.output_log.iter().map(|e| e.data.len()).sum(); if total > 100 * 1024 { while t.output_log.iter().map(|e| e.data.len()).sum::<usize>() > 50 * 1024 { t.output_log.remove(0); } } } }
        self.notify(id);
    }

    pub fn get_and_clear_output(&self, id: u64) -> Vec<OutputEntry> {
        let mut tasks = self.tasks.lock().unwrap();
        if let Some(t) = tasks.get_mut(&id) { std::mem::take(&mut t.output_log) } else { vec![] }
    }

    pub fn get_task_status(&self, id: u64) -> Option<(TaskStatus, Option<TaskResult>)> {
        self.tasks.lock().unwrap().get(&id).map(|t| (t.status.clone(), t.result.clone()))
    }

    pub fn get_task_session_id(&self, id: u64) -> Option<String> {
        self.tasks.lock().unwrap().get(&id).and_then(|t| t.session_id.clone())
    }

    pub fn delete_task(&self, id: u64) {
        self.tasks.lock().unwrap().remove(&id); self.notifiers.lock().unwrap().remove(&id);
    }

    pub fn list_tasks(&self) -> Vec<(u64, TaskStatus)> {
        self.tasks.lock().unwrap().values().map(|t| (t.id, t.status.clone())).collect()
    }

    pub fn drain_session_output(&self, session_id: &str) -> Vec<(u64, TaskStatus, Vec<OutputEntry>)> {
        let ids: Vec<u64> = { self.tasks.lock().unwrap().values().filter(|t| t.session_id.as_deref() == Some(session_id)).map(|t| t.id).collect() };
        let mut result = Vec::new();
        let mut tasks = self.tasks.lock().unwrap();
        for id in ids {
            if let Some(t) = tasks.get_mut(&id) {
                let output = std::mem::take(&mut t.output_log);
                result.push((t.id, t.status.clone(), output));
            }
        }
        result
    }

    fn notify(&self, id: u64) { if let Some(n) = self.notifiers.lock().unwrap().get(&id) { n.notify_waiters(); } }

    fn get_or_create_notifier(&self, id: u64) -> Arc<Notify> {
        self.notifiers.lock().unwrap().entry(id).or_insert_with(|| Arc::new(Notify::new())).clone()
    }

    pub async fn wait_for_output(&self, id: u64, timeout_ms: u64) -> bool {
        let notifier = self.get_or_create_notifier(id);
        let is_done = { self.tasks.lock().unwrap().get(&id).map(|t| matches!(t.status, TaskStatus::Completed | TaskStatus::Failed)).unwrap_or(true) };
        if is_done { return true; }
        tokio::time::timeout(Duration::from_millis(timeout_ms), notifier.notified()).await.is_ok()
    }

    pub async fn wait_for_completion(&self, id: u64) {
        loop {
            let notifier = self.get_or_create_notifier(id);
            let done_or_gone = { self.tasks.lock().unwrap().get(&id).map(|t| matches!(t.status, TaskStatus::Completed | TaskStatus::Failed)).unwrap_or(true) };
            if done_or_gone { return; }
            notifier.notified().await;
        }
    }

    pub fn cleanup_old_tasks(&self, active: &Arc<Mutex<HashMap<u64, (u32, Option<ChildStdin>)>>>) {
        let max_age = Duration::from_secs(30 * 60);
        let evicted: Vec<u64> = { self.tasks.lock().unwrap().values().filter(|t| t.completed_at.map(|c| c.elapsed() >= max_age).unwrap_or(false)).map(|t| t.id).collect() };
        { let mut a = active.lock().unwrap(); for id in &evicted { if let Some((pid, stdin)) = a.remove(id) { drop(stdin); crate::kill::kill_tree(pid); } } }
        self.tasks.lock().unwrap().retain(|_, t| t.completed_at.map(|c| c.elapsed() < max_age).unwrap_or(true));
    }
}

#[cfg(test)]
mod session_isolation_tests {
    use super::*;

    #[test]
    fn session_task_ids_filters_by_session() {
        let s = BackgroundTaskStore::new();
        let a1 = s.create_task(); s.set_session_id(a1, "session-A");
        let a2 = s.create_task(); s.set_session_id(a2, "session-A");
        let b1 = s.create_task(); s.set_session_id(b1, "session-B");
        let none = s.create_task();

        let mut a_ids = s.session_task_ids("session-A");
        a_ids.sort();
        assert_eq!(a_ids, vec![a1, a2]);

        assert_eq!(s.session_task_ids("session-B"), vec![b1]);
        assert!(s.session_task_ids("session-C").is_empty());
        assert!(s.session_task_ids("").is_empty());
        let _ = none;
    }

    #[test]
    fn delete_session_tasks_only_affects_target_session() {
        let s = BackgroundTaskStore::new();
        let a1 = s.create_task(); s.set_session_id(a1, "A");
        let a2 = s.create_task(); s.set_session_id(a2, "A");
        let b1 = s.create_task(); s.set_session_id(b1, "B");

        let deleted = s.delete_session_tasks("A");
        assert_eq!(deleted, 2);
        assert!(s.session_task_ids("A").is_empty());
        assert_eq!(s.session_task_ids("B"), vec![b1]);
    }

    #[test]
    fn delete_session_tasks_on_empty_session_is_zero() {
        let s = BackgroundTaskStore::new();
        let t = s.create_task(); s.set_session_id(t, "real");
        assert_eq!(s.delete_session_tasks("nonexistent"), 0);
        assert_eq!(s.session_task_ids("real"), vec![t]);
    }

    #[test]
    fn tasks_without_session_are_not_leaked_cross_session() {
        let s = BackgroundTaskStore::new();
        let orphan = s.create_task();
        let owned = s.create_task(); s.set_session_id(owned, "X");
        assert!(!s.session_task_ids("X").contains(&orphan));
        assert!(!s.session_task_ids("").contains(&orphan));
        assert!(!s.session_task_ids("").contains(&owned));
    }
}
