use agent_client_protocol as acp;
use serde_json::{Value, json};

use super::runtime::{TurnCompletion, TurnCompletionStatus};

pub(crate) fn parse_thread_id(result: &Value) -> acp::Result<String> {
    result
        .get("thread")
        .and_then(|v| v.get("id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            acp::Error::internal_error().data(format!(
                "codex app-server response missing thread.id: {result}"
            ))
        })
}

pub(crate) fn parse_turn_id(result: &Value) -> acp::Result<String> {
    result
        .get("turn")
        .and_then(|v| v.get("id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            acp::Error::internal_error().data(format!("codex response missing turn.id: {result}"))
        })
}

pub(crate) fn prompt_blocks_to_turn_input(prompt: &[acp::ContentBlock]) -> acp::Result<Vec<Value>> {
    let mut input = Vec::new();
    for block in prompt {
        let text = match block {
            acp::ContentBlock::Text(text) => Some(text.text.trim().to_string()),
            acp::ContentBlock::ResourceLink(resource) => {
                Some(format!("Referenced resource: {}", resource.uri))
            }
            acp::ContentBlock::Resource(resource) => match &resource.resource {
                acp::EmbeddedResourceResource::TextResourceContents(text) => {
                    Some(text.text.trim().to_string())
                }
                acp::EmbeddedResourceResource::BlobResourceContents(blob) => {
                    Some(format!("Embedded binary resource: {}", blob.uri))
                }
                _ => None,
            },
            acp::ContentBlock::Image(_) => Some("[image input omitted]".to_string()),
            acp::ContentBlock::Audio(_) => Some("[audio input omitted]".to_string()),
            _ => None,
        };
        if let Some(text) = text
            && !text.is_empty()
        {
            input.push(json!({ "type": "text", "text": text }));
        }
    }
    if input.is_empty() {
        return Err(
            acp::Error::invalid_params().data("prompt must contain at least one non-empty block")
        );
    }
    Ok(input)
}

pub(crate) fn map_stop_reason(completion: TurnCompletion) -> acp::Result<acp::PromptResponse> {
    match completion.status {
        TurnCompletionStatus::Completed => Ok(acp::PromptResponse::new(acp::StopReason::EndTurn)),
        TurnCompletionStatus::Interrupted => {
            Ok(acp::PromptResponse::new(acp::StopReason::Cancelled))
        }
        TurnCompletionStatus::Failed => Err(acp::Error::internal_error().data(
            completion
                .error_message
                .unwrap_or_else(|| "codex turn failed without error details".to_string()),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_blocks_to_turn_input_extracts_textual_content() {
        let blocks = vec![
            acp::ContentBlock::Text(acp::TextContent::new("hello")),
            acp::ContentBlock::ResourceLink(acp::ResourceLink::new(
                "temp-file",
                "file:///tmp/a.txt",
            )),
        ];
        let input = prompt_blocks_to_turn_input(&blocks).expect("input should parse");
        assert_eq!(input.len(), 2);
    }

    #[test]
    fn prompt_blocks_to_turn_input_rejects_empty_prompt() {
        let blocks = vec![acp::ContentBlock::Text(acp::TextContent::new("   "))];
        let err = prompt_blocks_to_turn_input(&blocks).expect_err("empty prompt should fail");
        assert_eq!(err.code, acp::ErrorCode::InvalidParams);
    }

    #[test]
    fn map_stop_reason_converts_known_states() {
        let ok = map_stop_reason(TurnCompletion {
            status: TurnCompletionStatus::Completed,
            error_message: None,
            turn_id: "turn-1".to_string(),
        })
        .expect("completed should map to end_turn");
        assert_eq!(ok.stop_reason, acp::StopReason::EndTurn);

        let cancelled = map_stop_reason(TurnCompletion {
            status: TurnCompletionStatus::Interrupted,
            error_message: None,
            turn_id: "turn-2".to_string(),
        })
        .expect("interrupted should map to cancelled");
        assert_eq!(cancelled.stop_reason, acp::StopReason::Cancelled);
    }
}
