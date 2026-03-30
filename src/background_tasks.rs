#![allow(dead_code)]
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

#[derive(Debug, Clone, PartialEq)]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone)]
pub struct OutputEntry {
    pub stream: String,
    pub data: String,
}

#[derive(Debug, Clone)]
pub struct TaskResult {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub error: Option<String>,
    pub exit_code: i32,
}

#[derive(Debug)]
pub struct Task {
    pub id: u64,
    pub status: TaskStatus,
    pub result: Option<TaskResult>,
    pub output_log: Vec<OutputEntry>,
    pub created_at: Instant,
    pub completed_at: Option<Instant>,
    pub session_id: Option<String>,
}

pub struct BackgroundTaskStore {
    tasks: Mutex<HashMap<u64, Task>>,
    counter: Mutex<u64>,
    notifiers: Mutex<HashMap<u64, Arc<Notify>>>,
}

impl BackgroundTaskStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            tasks: Mutex::new(HashMap::new()),
            counter: Mutex::new(0),
            notifiers: Mutex::new(HashMap::new()),
        })
    }

    pub fn create_task(&self) -> u64 {
        let mut c = self.counter.lock().unwrap();
        *c += 1;
        let id = *c;
        let mut tasks = self.tasks.lock().unwrap();
        tasks.insert(id, Task {
            id,
            status: TaskStatus::Pending,
            result: None,
            output_log: vec![],
            created_at: Instant::now(),
            completed_at: None,
            session_id: None,
        });
        id
    }

    pub fn set_session_id(&self, id: u64, session_id: &str) {
        let mut tasks = self.tasks.lock().unwrap();
        if let Some(t) = tasks.get_mut(&id) {
            t.session_id = Some(session_id.to_string());
        }
    }

    pub fn delete_session_tasks(&self, session_id: &str) -> u64 {
        let mut tasks = self.tasks.lock().unwrap();
        let ids: Vec<u64> = tasks.values()
            .filter(|t| t.session_id.as_deref() == Some(session_id))
            .map(|t| t.id)
            .collect();
        let count = ids.len() as u64;
        for id in &ids { tasks.remove(id); }
        drop(tasks);
        let mut notifiers = self.notifiers.lock().unwrap();
        for id in &ids { notifiers.remove(id); }
        count
    }

    pub fn start_task(&self, id: u64) {
        let mut tasks = self.tasks.lock().unwrap();
        if let Some(t) = tasks.get_mut(&id) {
            t.status = TaskStatus::Running;
        }
    }

    pub fn complete_task(&self, id: u64, result: TaskResult) {
        {
            let mut tasks = self.tasks.lock().unwrap();
            if let Some(t) = tasks.get_mut(&id) {
                t.status = TaskStatus::Completed;
                t.result = Some(result);
                t.completed_at = Some(Instant::now());
            }
        }
        self.notify(id);
    }

    pub fn fail_task(&self, id: u64, error: String) {
        {
            let mut tasks = self.tasks.lock().unwrap();
            if let Some(t) = tasks.get_mut(&id) {
                t.status = TaskStatus::Failed;
                t.result = Some(TaskResult { success: false, stdout: String::new(), stderr: String::new(), error: Some(error), exit_code: 1 });
                t.completed_at = Some(Instant::now());
            }
        }
        self.notify(id);
    }

    pub fn append_output(&self, id: u64, stream: &str, data: &str) {
        {
            let mut tasks = self.tasks.lock().unwrap();
            if let Some(t) = tasks.get_mut(&id) {
                t.output_log.push(OutputEntry { stream: stream.to_string(), data: data.to_string() });
                let total: usize = t.output_log.iter().map(|e| e.data.len()).sum();
                if total > 100 * 1024 {
                    while t.output_log.iter().map(|e| e.data.len()).sum::<usize>() > 50 * 1024 {
                        t.output_log.remove(0);
                    }
                }
            }
        }
        self.notify(id);
    }

    pub fn get_and_clear_output(&self, id: u64) -> Vec<OutputEntry> {
        let mut tasks = self.tasks.lock().unwrap();
        if let Some(t) = tasks.get_mut(&id) {
            std::mem::take(&mut t.output_log)
        } else {
            vec![]
        }
    }

    pub fn get_task_status(&self, id: u64) -> Option<(TaskStatus, Option<TaskResult>)> {
        let tasks = self.tasks.lock().unwrap();
        tasks.get(&id).map(|t| (t.status.clone(), t.result.clone()))
    }

    pub fn delete_task(&self, id: u64) {
        self.tasks.lock().unwrap().remove(&id);
        self.notifiers.lock().unwrap().remove(&id);
    }

    pub fn list_tasks(&self) -> Vec<(u64, TaskStatus)> {
        let tasks = self.tasks.lock().unwrap();
        tasks.values().map(|t| (t.id, t.status.clone())).collect()
    }

    fn notify(&self, id: u64) {
        let notifiers = self.notifiers.lock().unwrap();
        if let Some(n) = notifiers.get(&id) {
            n.notify_waiters();
        }
    }

    fn get_or_create_notifier(&self, id: u64) -> Arc<Notify> {
        let mut notifiers = self.notifiers.lock().unwrap();
        notifiers.entry(id).or_insert_with(|| Arc::new(Notify::new())).clone()
    }

    pub async fn wait_for_output(&self, id: u64, timeout_ms: u64) -> bool {
        let notifier = self.get_or_create_notifier(id);
        let is_done = {
            let tasks = self.tasks.lock().unwrap();
            tasks.get(&id).map(|t| matches!(t.status, TaskStatus::Completed | TaskStatus::Failed)).unwrap_or(true)
        };
        if is_done { return true; }
        tokio::time::timeout(Duration::from_millis(timeout_ms), notifier.notified()).await.is_ok()
    }

    pub fn cleanup_old_tasks(&self) {
        let max_age = Duration::from_secs(30 * 60);
        let mut tasks = self.tasks.lock().unwrap();
        tasks.retain(|_, t| {
            if let Some(completed_at) = t.completed_at {
                completed_at.elapsed() < max_age
            } else {
                true
            }
        });
    }
}
