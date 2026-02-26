use std::path::PathBuf;

use crate::acp::AcpBackend;

use super::{
    CALLBACK_STOP_PREFIX, parse_new_task_args, parse_prompt_command, parse_task_action,
    render_stream_text,
};

#[test]
fn parse_prompt_command_ok() {
    let parsed = parse_prompt_command("/prompt 42 hello world").expect("parse should work");
    assert_eq!(parsed.0, 42);
    assert_eq!(parsed.1, "hello world");
}

#[test]
fn parse_task_action_ok() {
    let task = parse_task_action("stop:9", CALLBACK_STOP_PREFIX).expect("parse should work");
    assert_eq!(task, 9);
}

#[test]
fn render_stream_text_truncates() {
    let text = "x".repeat(200);
    let rendered = render_stream_text(1, &text, 60);
    assert!(rendered.contains("已截断"));
}

#[test]
fn parse_new_task_args_requires_path() {
    assert!(parse_new_task_args("/new").is_none());
    assert!(parse_new_task_args("/new   ").is_none());
    assert!(parse_new_task_args("/new ./workspace").is_none());
}

#[test]
fn parse_new_task_args_accepts_path_and_model() {
    let parsed = parse_new_task_args("/new ./workspace model=gpt-5-codex backend=codex")
        .expect("args should parse");
    assert_eq!(parsed.0, PathBuf::from("./workspace"));
    assert_eq!(parsed.1, AcpBackend::Codex);
    assert_eq!(parsed.2.as_deref(), Some("gpt-5-codex"));
}

#[test]
fn parse_new_task_args_accepts_path_without_model() {
    let parsed = parse_new_task_args("/new ./workspace backend=codex").expect("args should parse");
    assert_eq!(parsed.0, PathBuf::from("./workspace"));
    assert_eq!(parsed.1, AcpBackend::Codex);
    assert!(parsed.2.is_none());
}

#[test]
fn parse_new_task_args_rejects_unknown_backend() {
    assert!(parse_new_task_args("/new ./workspace backend=unknown").is_none());
}
