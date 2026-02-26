use crate::task_types::{TaskId, TaskSnapshot, TaskState};

pub(super) fn format_task_summary(task: &TaskSnapshot) -> String {
    format!(
        "#{} | 状态: {} | cwd: {}",
        task.task_id,
        state_label(&task.state),
        task.cwd.display()
    )
}

pub(super) fn render_stream_text(task_id: TaskId, text: &str, max_chars: usize) -> String {
    let clipped = if text.chars().count() > max_chars {
        let tail_len = max_chars.saturating_sub(32);
        let tail: String = text
            .chars()
            .rev()
            .take(tail_len)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        format!("...（已截断）\n{tail}")
    } else {
        text.to_string()
    };

    format!("任务 {task_id} 输出:\n{clipped}")
}

fn state_label(state: &TaskState) -> &'static str {
    match state {
        TaskState::Starting => "starting",
        TaskState::Idle => "idle",
        TaskState::Running => "running",
        TaskState::Finished => "finished",
        TaskState::Failed => "failed",
    }
}
