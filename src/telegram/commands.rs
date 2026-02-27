use teloxide::requests::Requester;
use teloxide::types::BotCommand;
use teloxide::{Bot, RequestError};

pub(super) async fn register_bot_commands(bot: &Bot) -> Result<(), RequestError> {
    bot.set_my_commands(supported_commands()).await?;
    Ok(())
}

fn supported_commands() -> Vec<BotCommand> {
    vec![
        BotCommand::new("start", "Show usage and quick actions"),
        BotCommand::new("new", "Create a task: /new <cwd> backend=codex [model=xxx]"),
        BotCommand::new("list", "List tasks in the current chat"),
        BotCommand::new("prompt", "Send prompt to task: /prompt <task_id> <content>"),
    ]
}

#[cfg(test)]
mod tests {
    use super::supported_commands;

    #[test]
    fn supported_commands_are_complete_and_ordered() {
        let commands = supported_commands();
        let command_names: Vec<&str> = commands
            .iter()
            .map(|command| command.command.as_str())
            .collect();
        assert_eq!(command_names, vec!["start", "new", "list", "prompt"]);
    }

    #[test]
    fn supported_commands_have_valid_descriptions() {
        for command in supported_commands() {
            assert!(
                command.description.len() >= 3,
                "description too short for command {}",
                command.command
            );
            assert!(
                command.description.len() <= 256,
                "description too long for command {}",
                command.command
            );
        }
    }
}
