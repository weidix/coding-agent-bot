use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use teloxide::prelude::*;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup, Me, MessageId, User};
use tokio::sync::Mutex;

use crate::acp::AcpBackend;
use crate::config::TelegramConfig;
use crate::task_manager::{TaskManager, resolve_cwd};
use crate::task_types::{TaskEvent, TaskEventKind, TaskId, TaskSnapshot};
use crate::whitelist::AccessControl;
use render::{format_task_summary, render_stream_text};

mod render;

const CALLBACK_NEW_TASK: &str = "new_task";
const CALLBACK_LIST_TASKS: &str = "list_tasks";
const CALLBACK_STOP_PREFIX: &str = "stop:";
const CALLBACK_STATUS_PREFIX: &str = "status:";
const NEW_TASK_USAGE: &str = "/new <cwd> backend=codex [model=xxx]";
const NEW_TASK_HINT: &str = "请使用 /new <cwd> backend=codex [model=xxx] 创建任务。";
const START_HELP_TEXT: &str = "可用操作：\n1) 使用 /new <cwd> backend=codex [model=xxx] 新建任务\n2) 发送内容给最近任务\n3) /prompt <task_id> <内容>\n4) /list 查看任务";

#[derive(Clone)]
pub struct TelegramRuntime {
    manager: TaskManager,
    access_control: AccessControl,
    config: TelegramConfig,
}

#[derive(Debug, Default)]
struct StreamRenderer {
    by_task: HashMap<TaskId, StreamState>,
}

#[derive(Debug)]
struct StreamState {
    chat_id: i64,
    message_id: MessageId,
    content: String,
    last_edit: Instant,
}

pub async fn run_telegram(runtime: TelegramRuntime, bot: Bot) {
    let stream_renderer = Arc::new(Mutex::new(StreamRenderer::default()));

    spawn_stream_updates(
        bot.clone(),
        runtime.manager.clone(),
        stream_renderer.clone(),
        runtime.config.clone(),
    );

    let handler = dptree::entry()
        .branch(Update::filter_message().endpoint(handle_message))
        .branch(Update::filter_callback_query().endpoint(handle_callback));

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![runtime, stream_renderer])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
}

impl TelegramRuntime {
    pub fn new(
        manager: TaskManager,
        access_control: AccessControl,
        config: TelegramConfig,
    ) -> Self {
        Self {
            manager,
            access_control,
            config,
        }
    }
}

async fn handle_message(
    bot: Bot,
    _me: Me,
    msg: Message,
    runtime: TelegramRuntime,
    _stream_renderer: Arc<Mutex<StreamRenderer>>,
) -> Result<(), teloxide::RequestError> {
    let Some(text) = msg.text() else {
        return Ok(());
    };

    let chat_id = msg.chat.id.0;
    let user_id = match user_id_from_message(msg.from.as_ref()) {
        Some(id) => id,
        None => {
            bot.send_message(msg.chat.id, "无法识别用户。请在私聊中使用。")
                .await?;
            return Ok(());
        }
    };

    if !runtime
        .access_control
        .is_user_allowed(Some(user_id), chat_id)
    {
        bot.send_message(msg.chat.id, "不在白名单中，已拒绝此操作。")
            .await?;
        return Ok(());
    }

    if text.starts_with("/start") {
        bot.send_message(msg.chat.id, START_HELP_TEXT)
            .reply_markup(main_keyboard())
            .await?;
        return Ok(());
    }

    if text.starts_with("/new") {
        let args = parse_new_task_args(text);
        let Some((cwd, backend, model)) = args else {
            bot.send_message(msg.chat.id, format!("命令格式：{NEW_TASK_USAGE}"))
                .await?;
            return Ok(());
        };
        match runtime
            .manager
            .create_task(chat_id, user_id, Some(cwd), backend, model)
            .await
        {
            Ok(task) => {
                bot.send_message(
                    msg.chat.id,
                    format!("任务 {} 已创建，目录：{}", task.task_id, task.cwd.display()),
                )
                .reply_markup(task_keyboard(task.task_id))
                .await?;
            }
            Err(err) => {
                bot.send_message(msg.chat.id, format!("创建任务失败：{err}"))
                    .await?;
            }
        }
        return Ok(());
    }

    if text.starts_with("/list") {
        let tasks = runtime
            .manager
            .list_tasks(Some(chat_id), Some(user_id))
            .await;
        bot.send_message(msg.chat.id, render_task_list(&tasks))
            .reply_markup(main_keyboard())
            .await?;
        return Ok(());
    }

    if text.starts_with("/prompt") {
        if let Some((task_id, prompt)) = parse_prompt_command(text) {
            if let Err(err) = runtime.manager.submit_prompt(task_id, prompt).await {
                bot.send_message(msg.chat.id, format!("提交失败：{err}"))
                    .await?;
            }
        } else {
            bot.send_message(msg.chat.id, "命令格式：/prompt <task_id> <内容>")
                .await?;
        }
        return Ok(());
    }

    let task = match runtime.manager.default_task_for(chat_id, user_id).await {
        Some(task) => task,
        None => {
            bot.send_message(msg.chat.id, NEW_TASK_HINT).await?;
            return Ok(());
        }
    };

    if let Err(err) = runtime
        .manager
        .submit_prompt(task.task_id, text.to_string())
        .await
    {
        bot.send_message(msg.chat.id, format!("提交失败：{err}"))
            .await?;
    }

    Ok(())
}

async fn handle_callback(
    bot: Bot,
    q: CallbackQuery,
    runtime: TelegramRuntime,
    _stream_renderer: Arc<Mutex<StreamRenderer>>,
) -> Result<(), teloxide::RequestError> {
    let Some(data) = &q.data else {
        return Ok(());
    };

    let (chat_id, user_id) = match extract_callback_context(&q) {
        Some(ctx) => ctx,
        None => {
            bot.answer_callback_query(q.id).await?;
            return Ok(());
        }
    };

    if !runtime
        .access_control
        .is_user_allowed(Some(user_id), chat_id)
    {
        bot.answer_callback_query(q.id.clone())
            .text("不在白名单中")
            .await?;
        return Ok(());
    }

    if data == CALLBACK_NEW_TASK {
        send_or_edit_callback_text(&bot, &q, NEW_TASK_HINT.to_string(), Some(main_keyboard()))
            .await?;
    } else if data == CALLBACK_LIST_TASKS {
        let tasks = runtime
            .manager
            .list_tasks(Some(chat_id), Some(user_id))
            .await;
        send_or_edit_callback_text(&bot, &q, render_task_list(&tasks), Some(main_keyboard()))
            .await?;
    } else if let Some(task_id) = parse_task_action(data, CALLBACK_STOP_PREFIX) {
        let result = runtime.manager.cancel_task(task_id).await;
        let text = match result {
            Ok(()) => format!("任务 {task_id} 已发送取消请求"),
            Err(err) => format!("取消失败：{err}"),
        };
        send_or_edit_callback_text(&bot, &q, text, Some(task_keyboard(task_id))).await?;
    } else if let Some(task_id) = parse_task_action(data, CALLBACK_STATUS_PREFIX) {
        let text = match runtime.manager.task_snapshot(task_id).await {
            Some(task) => format_task_summary(&task),
            None => format!("任务 {task_id} 不存在"),
        };
        send_or_edit_callback_text(&bot, &q, text, Some(task_keyboard(task_id))).await?;
    }

    bot.answer_callback_query(q.id).await?;
    Ok(())
}

fn spawn_stream_updates(
    bot: Bot,
    manager: TaskManager,
    renderer: Arc<Mutex<StreamRenderer>>,
    config: TelegramConfig,
) {
    tokio::spawn(async move {
        let mut rx = manager.subscribe_events();
        while let Ok(event) = rx.recv().await {
            if let Err(err) = handle_task_event(&bot, &renderer, &config, event).await {
                tracing::error!("telegram stream update failed: {err}");
            }
        }
    });
}

async fn handle_task_event(
    bot: &Bot,
    renderer: &Arc<Mutex<StreamRenderer>>,
    config: &TelegramConfig,
    event: TaskEvent,
) -> Result<()> {
    match event.kind {
        TaskEventKind::StartedPrompt => {
            let sent = bot
                .send_message(
                    ChatId(event.chat_id),
                    format!("任务 {} 正在执行...", event.task_id),
                )
                .reply_markup(task_keyboard(event.task_id))
                .await?;

            let mut lock = renderer.lock().await;
            lock.by_task.insert(
                event.task_id,
                StreamState {
                    chat_id: event.chat_id,
                    message_id: sent.id,
                    content: String::new(),
                    last_edit: Instant::now(),
                },
            );
        }
        TaskEventKind::OutputChunk { text } => {
            let mut lock = renderer.lock().await;
            if let Some(state) = lock.by_task.get_mut(&event.task_id) {
                state.content.push_str(&text);
                if state.last_edit.elapsed()
                    >= Duration::from_millis(config.stream_edit_interval_ms)
                {
                    let rendered =
                        render_stream_text(event.task_id, &state.content, config.message_max_chars);
                    bot.edit_message_text(ChatId(state.chat_id), state.message_id, rendered)
                        .reply_markup(task_keyboard(event.task_id))
                        .await?;
                    state.last_edit = Instant::now();
                }
            }
        }
        TaskEventKind::ToolUpdate { text } => {
            let mut lock = renderer.lock().await;
            if let Some(state) = lock.by_task.get_mut(&event.task_id) {
                state.content.push_str("\n");
                state.content.push_str(&text);
            }
        }
        TaskEventKind::Finished { stop_reason } => {
            flush_and_close_stream(
                bot,
                renderer,
                event.task_id,
                format!("\n\n完成，停止原因：{stop_reason}"),
                config.message_max_chars,
            )
            .await?;
        }
        TaskEventKind::Error { message } => {
            flush_and_close_stream(
                bot,
                renderer,
                event.task_id,
                format!("\n\n错误：{message}"),
                config.message_max_chars,
            )
            .await?;
        }
        TaskEventKind::Cancelled => {
            flush_and_close_stream(
                bot,
                renderer,
                event.task_id,
                "\n\n已取消".to_string(),
                config.message_max_chars,
            )
            .await?;
        }
        TaskEventKind::Created => {}
    }

    Ok(())
}

async fn flush_and_close_stream(
    bot: &Bot,
    renderer: &Arc<Mutex<StreamRenderer>>,
    task_id: TaskId,
    suffix: String,
    max_chars: usize,
) -> Result<()> {
    let mut lock = renderer.lock().await;
    if let Some(state) = lock.by_task.remove(&task_id) {
        let mut output = state.content;
        output.push_str(&suffix);
        let rendered = render_stream_text(task_id, &output, max_chars);
        bot.edit_message_text(ChatId(state.chat_id), state.message_id, rendered)
            .reply_markup(task_keyboard(task_id))
            .await?;
    }

    Ok(())
}

async fn send_or_edit_callback_text(
    bot: &Bot,
    q: &CallbackQuery,
    text: String,
    markup: Option<InlineKeyboardMarkup>,
) -> Result<(), teloxide::RequestError> {
    if let Some(message) = q.regular_message() {
        let mut req = bot.edit_message_text(message.chat.id, message.id, text);
        if let Some(markup) = markup {
            req = req.reply_markup(markup);
        }
        req.await?;
    }

    Ok(())
}

fn parse_prompt_command(input: &str) -> Option<(TaskId, String)> {
    let mut parts = input.splitn(3, ' ');
    let _ = parts.next()?;
    let task_id = parts.next()?.parse::<TaskId>().ok()?;
    let prompt = parts.next()?.trim().to_string();
    if prompt.is_empty() {
        return None;
    }

    Some((task_id, prompt))
}

fn parse_new_task_args(input: &str) -> Option<(PathBuf, AcpBackend, Option<String>)> {
    if !input.starts_with("/new") {
        return None;
    }

    let raw = input.split_once(' ').map(|(_, rhs)| rhs.trim())?;
    if raw.is_empty() {
        return None;
    }

    let mut parts = raw.split_whitespace();
    let cwd = resolve_cwd(parts.next())?;
    let mut backend = None;
    let mut model = None;

    for token in parts {
        if let Some(value) = token.strip_prefix("backend=") {
            if backend.is_some() {
                return None;
            }
            backend = value.parse::<AcpBackend>().ok();
            if backend.is_none() {
                return None;
            }
            continue;
        }

        if let Some(value) = token.strip_prefix("model=") {
            if model.is_some() || value.trim().is_empty() {
                return None;
            }
            model = Some(value.to_string());
            continue;
        }

        if model.is_none() {
            model = Some(token.to_string());
        } else {
            return None;
        }
    }

    Some((cwd, backend?, model))
}

fn parse_task_action(data: &str, prefix: &str) -> Option<TaskId> {
    data.strip_prefix(prefix)?.parse::<TaskId>().ok()
}

fn extract_callback_context(query: &CallbackQuery) -> Option<(i64, i64)> {
    let chat_id = query.regular_message()?.chat.id.0;
    let user_id = user_id_from_user(&query.from)?;
    Some((chat_id, user_id))
}

fn user_id_from_message(user: Option<&User>) -> Option<i64> {
    user.and_then(user_id_from_user)
}

fn user_id_from_user(user: &User) -> Option<i64> {
    i64::try_from(user.id.0).ok()
}

fn main_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback("新建任务", CALLBACK_NEW_TASK),
        InlineKeyboardButton::callback("任务列表", CALLBACK_LIST_TASKS),
    ]])
}

fn task_keyboard(task_id: TaskId) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback("停止", format!("{CALLBACK_STOP_PREFIX}{task_id}")),
        InlineKeyboardButton::callback("状态", format!("{CALLBACK_STATUS_PREFIX}{task_id}")),
    ]])
}

fn render_task_list(tasks: &[TaskSnapshot]) -> String {
    if tasks.is_empty() {
        return "当前没有任务。".to_string();
    }

    let mut lines = Vec::with_capacity(tasks.len() + 1);
    lines.push("任务列表：".to_string());
    for task in tasks {
        lines.push(format_task_summary(task));
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests;
