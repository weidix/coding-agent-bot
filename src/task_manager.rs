use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Result, anyhow};
use tokio::sync::{Mutex, broadcast, mpsc};

use crate::acp::{AcpBackend, AcpRuntimeConfig, AcpWorkerCommand, EventEmitter, spawn_acp_worker};
use crate::task_types::{TaskEvent, TaskId, TaskSnapshot, TaskState};
use crate::whitelist::AccessControl;

#[derive(Clone)]
pub struct TaskManager {
    inner: Arc<TaskManagerInner>,
}

struct TaskManagerInner {
    runtime_cfg: AcpRuntimeConfig,
    access_control: AccessControl,
    max_running_tasks: usize,
    next_task_id: AtomicU64,
    sequence: Arc<AtomicU64>,
    tasks: Mutex<HashMap<TaskId, ManagedTask>>,
    event_tx: broadcast::Sender<TaskEvent>,
}

struct ManagedTask {
    snapshot: TaskSnapshot,
    command_tx: mpsc::UnboundedSender<AcpWorkerCommand>,
}

impl TaskManager {
    pub fn new(
        runtime_cfg: AcpRuntimeConfig,
        access_control: AccessControl,
        max_running_tasks: usize,
    ) -> Self {
        let (event_tx, _) = broadcast::channel(512);
        let inner = Arc::new(TaskManagerInner {
            runtime_cfg,
            access_control,
            max_running_tasks,
            next_task_id: AtomicU64::new(1),
            sequence: Arc::new(AtomicU64::new(1)),
            tasks: Mutex::new(HashMap::new()),
            event_tx,
        });

        let manager = Self { inner };
        manager.spawn_state_sync();
        manager
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<TaskEvent> {
        self.inner.event_tx.subscribe()
    }

    pub async fn create_task(
        &self,
        chat_id: i64,
        user_id: i64,
        cwd: Option<PathBuf>,
        backend: AcpBackend,
        model: Option<String>,
    ) -> Result<TaskSnapshot> {
        let mut tasks = self.inner.tasks.lock().await;
        if tasks.len() >= self.inner.max_running_tasks {
            return Err(anyhow!(
                "maximum running tasks reached: {}",
                self.inner.max_running_tasks
            ));
        }

        let base_dir = std::env::current_dir()?;
        let chosen_cwd = cwd.ok_or_else(|| anyhow!("cwd must be provided when creating a task"))?;
        let chosen_cwd = if chosen_cwd.is_absolute() {
            chosen_cwd
        } else {
            base_dir.join(chosen_cwd)
        };
        if !self
            .inner
            .access_control
            .is_path_allowed(&chosen_cwd, &base_dir)
        {
            return Err(anyhow!(
                "cwd '{}' is not allowed by whitelist folders",
                chosen_cwd.display()
            ));
        }

        let task_id = self.inner.next_task_id.fetch_add(1, Ordering::Relaxed);
        let event_emitter = EventEmitter::new(
            self.inner.sequence.clone(),
            self.inner.event_tx.clone(),
            task_id,
            chat_id,
            user_id,
        );

        let mut runtime_cfg = self.inner.runtime_cfg.clone();
        runtime_cfg.backend = backend;
        runtime_cfg.codex_model = model;

        let worker_tx = spawn_acp_worker(
            runtime_cfg,
            task_id,
            chosen_cwd.clone(),
            self.inner.access_control.clone(),
            event_emitter,
        )?;

        let snapshot = TaskSnapshot {
            task_id,
            chat_id,
            user_id,
            cwd: chosen_cwd,
            state: TaskState::Starting,
        };

        tasks.insert(
            task_id,
            ManagedTask {
                snapshot: snapshot.clone(),
                command_tx: worker_tx,
            },
        );

        Ok(snapshot)
    }

    pub async fn submit_prompt(&self, task_id: TaskId, prompt: String) -> Result<()> {
        if prompt.trim().is_empty() {
            return Err(anyhow!("prompt cannot be empty"));
        }

        let mut tasks = self.inner.tasks.lock().await;
        let task = tasks
            .get_mut(&task_id)
            .ok_or_else(|| anyhow!("task {task_id} not found"))?;

        task.snapshot.state = TaskState::Running;
        task.command_tx
            .send(AcpWorkerCommand::Prompt(prompt))
            .map_err(|_| anyhow!("task {task_id} command channel is closed"))
    }

    pub async fn cancel_task(&self, task_id: TaskId) -> Result<()> {
        let tasks = self.inner.tasks.lock().await;
        let task = tasks
            .get(&task_id)
            .ok_or_else(|| anyhow!("task {task_id} not found"))?;

        task.command_tx
            .send(AcpWorkerCommand::Cancel)
            .map_err(|_| anyhow!("task {task_id} command channel is closed"))
    }

    pub async fn shutdown_task(&self, task_id: TaskId) -> Result<()> {
        let mut tasks = self.inner.tasks.lock().await;
        let task = tasks
            .remove(&task_id)
            .ok_or_else(|| anyhow!("task {task_id} not found"))?;

        let _ = task.command_tx.send(AcpWorkerCommand::Shutdown);
        Ok(())
    }

    pub async fn list_tasks(
        &self,
        chat_id: Option<i64>,
        user_id: Option<i64>,
    ) -> Vec<TaskSnapshot> {
        let tasks = self.inner.tasks.lock().await;

        tasks
            .values()
            .filter(|task| {
                let chat_ok = chat_id
                    .map(|id| id == task.snapshot.chat_id)
                    .unwrap_or(true);
                let user_ok = user_id
                    .map(|id| id == task.snapshot.user_id)
                    .unwrap_or(true);
                chat_ok && user_ok
            })
            .map(|task| task.snapshot.clone())
            .collect()
    }

    pub async fn task_snapshot(&self, task_id: TaskId) -> Option<TaskSnapshot> {
        let tasks = self.inner.tasks.lock().await;
        tasks.get(&task_id).map(|task| task.snapshot.clone())
    }

    pub async fn default_task_for(&self, chat_id: i64, user_id: i64) -> Option<TaskSnapshot> {
        let tasks = self.inner.tasks.lock().await;
        tasks
            .values()
            .filter(|task| task.snapshot.chat_id == chat_id && task.snapshot.user_id == user_id)
            .max_by_key(|task| task.snapshot.task_id)
            .map(|task| task.snapshot.clone())
    }

    pub fn access_control(&self) -> AccessControl {
        self.inner.access_control.clone()
    }

    fn spawn_state_sync(&self) {
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let mut rx = inner.event_tx.subscribe();
            while let Ok(event) = rx.recv().await {
                let mut tasks = inner.tasks.lock().await;
                if let Some(task) = tasks.get_mut(&event.task_id) {
                    task.snapshot.state = next_state_from_event(&event, &task.snapshot.state);
                }
            }
        });
    }
}

fn next_state_from_event(event: &TaskEvent, current: &TaskState) -> TaskState {
    match &event.kind {
        crate::task_types::TaskEventKind::Created => TaskState::Idle,
        crate::task_types::TaskEventKind::StartedPrompt
        | crate::task_types::TaskEventKind::OutputChunk { .. }
        | crate::task_types::TaskEventKind::ToolUpdate { .. } => TaskState::Running,
        crate::task_types::TaskEventKind::Finished { .. }
        | crate::task_types::TaskEventKind::Cancelled => TaskState::Idle,
        crate::task_types::TaskEventKind::Error { .. } => match current {
            TaskState::Running | TaskState::Starting => TaskState::Failed,
            _ => current.clone(),
        },
    }
}

pub fn resolve_cwd(input: Option<&str>) -> Option<PathBuf> {
    match input {
        Some(raw) if !raw.trim().is_empty() => Some(PathBuf::from(raw.trim())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WhitelistConfig;
    use crate::task_types::TaskEventKind;
    use std::path::Path;

    #[test]
    fn resolve_cwd_uses_input_when_present() {
        let cwd = resolve_cwd(Some("./workspace"));
        assert_eq!(cwd, Some(PathBuf::from("./workspace")));
    }

    #[test]
    fn resolve_cwd_returns_none_when_empty() {
        let cwd = resolve_cwd(Some(""));
        assert!(cwd.is_none());
    }

    #[test]
    fn state_transitions() {
        let event = TaskEvent {
            sequence: 1,
            task_id: 1,
            chat_id: 1,
            user_id: 1,
            kind: TaskEventKind::StartedPrompt,
        };

        let state = next_state_from_event(&event, &TaskState::Idle);
        assert_eq!(state, TaskState::Running);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_task_requires_explicit_cwd() {
        let access_control =
            AccessControl::from_config(&WhitelistConfig::default(), Path::new("."))
                .expect("access control");
        let manager = TaskManager::new(AcpRuntimeConfig::default(), access_control, 1);

        let err = manager
            .create_task(1001, 2001, None, AcpBackend::Codex, None)
            .await
            .expect_err("missing cwd should fail");
        assert!(
            err.to_string().contains("cwd must be provided"),
            "unexpected error: {err}"
        );
    }
}
