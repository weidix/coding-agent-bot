use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub type TaskId = u64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Starting,
    Idle,
    Running,
    Finished,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskEventKind {
    Created,
    StartedPrompt,
    OutputChunk { text: String },
    ToolUpdate { text: String },
    Finished { stop_reason: String },
    Error { message: String },
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEvent {
    pub sequence: u64,
    pub task_id: TaskId,
    pub chat_id: i64,
    pub user_id: i64,
    pub kind: TaskEventKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSnapshot {
    pub task_id: TaskId,
    pub chat_id: i64,
    pub user_id: i64,
    pub cwd: PathBuf,
    pub state: TaskState,
}
