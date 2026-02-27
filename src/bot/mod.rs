use agent_client_protocol as acp;
use async_trait::async_trait;

pub mod telegram;

/// ACP-level abilities that a bot client may provide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BotAbility {
    RequestPermission,
    SessionNotification,
    FsReadTextFile,
    FsWriteTextFile,
    TerminalCreate,
    TerminalOutput,
    TerminalRelease,
    TerminalWaitForExit,
    TerminalKill,
    ExtMethod,
    ExtNotification,
}

/// Build the required ability list from ACP `ClientCapabilities`.
#[must_use]
pub fn required_abilities(capabilities: &acp::ClientCapabilities) -> Vec<BotAbility> {
    let mut abilities = vec![
        BotAbility::RequestPermission,
        BotAbility::SessionNotification,
    ];

    if capabilities.fs.read_text_file {
        abilities.push(BotAbility::FsReadTextFile);
    }
    if capabilities.fs.write_text_file {
        abilities.push(BotAbility::FsWriteTextFile);
    }
    if capabilities.terminal {
        abilities.extend([
            BotAbility::TerminalCreate,
            BotAbility::TerminalOutput,
            BotAbility::TerminalRelease,
            BotAbility::TerminalWaitForExit,
            BotAbility::TerminalKill,
        ]);
    }

    abilities
}

/// Abilities that are always optional and not negotiated via `ClientCapabilities`.
pub const OPTIONAL_ABILITIES: [BotAbility; 2] =
    [BotAbility::ExtMethod, BotAbility::ExtNotification];

/// Unified bot interface for ACP client-side responsibilities.
///
/// Method signatures intentionally reuse ACP schema types directly.
#[async_trait(?Send)]
pub trait Bot {
    /// Declares ACP capabilities that this bot exposes to the agent.
    fn capabilities(&self) -> acp::ClientCapabilities;

    /// Required: permission request handling.
    async fn request_permission(
        &self,
        args: acp::RequestPermissionRequest,
    ) -> acp::Result<acp::RequestPermissionResponse>;

    /// Required: session update handling.
    async fn session_notification(&self, args: acp::SessionNotification) -> acp::Result<()>;

    /// Optional: `fs.write_text_file`.
    async fn write_text_file(
        &self,
        _args: acp::WriteTextFileRequest,
    ) -> acp::Result<acp::WriteTextFileResponse> {
        Err(acp::Error::method_not_found())
    }

    /// Optional: `fs.read_text_file`.
    async fn read_text_file(
        &self,
        _args: acp::ReadTextFileRequest,
    ) -> acp::Result<acp::ReadTextFileResponse> {
        Err(acp::Error::method_not_found())
    }

    /// Optional: `terminal.create`.
    async fn create_terminal(
        &self,
        _args: acp::CreateTerminalRequest,
    ) -> acp::Result<acp::CreateTerminalResponse> {
        Err(acp::Error::method_not_found())
    }

    /// Optional: `terminal.output`.
    async fn terminal_output(
        &self,
        _args: acp::TerminalOutputRequest,
    ) -> acp::Result<acp::TerminalOutputResponse> {
        Err(acp::Error::method_not_found())
    }

    /// Optional: `terminal.release`.
    async fn release_terminal(
        &self,
        _args: acp::ReleaseTerminalRequest,
    ) -> acp::Result<acp::ReleaseTerminalResponse> {
        Err(acp::Error::method_not_found())
    }

    /// Optional: `terminal.wait_for_exit`.
    async fn wait_for_terminal_exit(
        &self,
        _args: acp::WaitForTerminalExitRequest,
    ) -> acp::Result<acp::WaitForTerminalExitResponse> {
        Err(acp::Error::method_not_found())
    }

    /// Optional: `terminal.kill`.
    async fn kill_terminal_command(
        &self,
        _args: acp::KillTerminalCommandRequest,
    ) -> acp::Result<acp::KillTerminalCommandResponse> {
        Err(acp::Error::method_not_found())
    }

    /// Optional: ACP extension request.
    async fn ext_method(&self, _args: acp::ExtRequest) -> acp::Result<acp::ExtResponse> {
        Ok(acp::ExtResponse::new(acp::RawValue::NULL.to_owned().into()))
    }

    /// Optional: ACP extension notification.
    async fn ext_notification(&self, _args: acp::ExtNotification) -> acp::Result<()> {
        Ok(())
    }
}

/// Adapter that exposes a `Bot` implementation as ACP `Client`.
#[derive(Debug, Clone)]
pub struct AcpClientAdapter<B> {
    inner: B,
}

impl<B> AcpClientAdapter<B> {
    #[must_use]
    pub fn new(inner: B) -> Self {
        Self { inner }
    }

    #[must_use]
    pub fn capabilities(&self) -> acp::ClientCapabilities
    where
        B: Bot,
    {
        self.inner.capabilities()
    }

    #[must_use]
    pub fn into_inner(self) -> B {
        self.inner
    }
}

#[async_trait(?Send)]
impl<B: Bot> acp::Client for AcpClientAdapter<B> {
    async fn request_permission(
        &self,
        args: acp::RequestPermissionRequest,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        self.inner.request_permission(args).await
    }

    async fn session_notification(&self, args: acp::SessionNotification) -> acp::Result<()> {
        self.inner.session_notification(args).await
    }

    async fn write_text_file(
        &self,
        args: acp::WriteTextFileRequest,
    ) -> acp::Result<acp::WriteTextFileResponse> {
        self.inner.write_text_file(args).await
    }

    async fn read_text_file(
        &self,
        args: acp::ReadTextFileRequest,
    ) -> acp::Result<acp::ReadTextFileResponse> {
        self.inner.read_text_file(args).await
    }

    async fn create_terminal(
        &self,
        args: acp::CreateTerminalRequest,
    ) -> acp::Result<acp::CreateTerminalResponse> {
        self.inner.create_terminal(args).await
    }

    async fn terminal_output(
        &self,
        args: acp::TerminalOutputRequest,
    ) -> acp::Result<acp::TerminalOutputResponse> {
        self.inner.terminal_output(args).await
    }

    async fn release_terminal(
        &self,
        args: acp::ReleaseTerminalRequest,
    ) -> acp::Result<acp::ReleaseTerminalResponse> {
        self.inner.release_terminal(args).await
    }

    async fn wait_for_terminal_exit(
        &self,
        args: acp::WaitForTerminalExitRequest,
    ) -> acp::Result<acp::WaitForTerminalExitResponse> {
        self.inner.wait_for_terminal_exit(args).await
    }

    async fn kill_terminal_command(
        &self,
        args: acp::KillTerminalCommandRequest,
    ) -> acp::Result<acp::KillTerminalCommandResponse> {
        self.inner.kill_terminal_command(args).await
    }

    async fn ext_method(&self, args: acp::ExtRequest) -> acp::Result<acp::ExtResponse> {
        self.inner.ext_method(args).await
    }

    async fn ext_notification(&self, args: acp::ExtNotification) -> acp::Result<()> {
        self.inner.ext_notification(args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_abilities_include_only_mandatory_with_default_capabilities() {
        let abilities = required_abilities(&acp::ClientCapabilities::default());
        assert_eq!(
            abilities,
            vec![
                BotAbility::RequestPermission,
                BotAbility::SessionNotification
            ]
        );
    }

    #[test]
    fn required_abilities_expand_with_fs_and_terminal_capabilities() {
        let capabilities = acp::ClientCapabilities::new()
            .fs(acp::FileSystemCapability::new()
                .read_text_file(true)
                .write_text_file(true))
            .terminal(true);
        let abilities = required_abilities(&capabilities);
        assert_eq!(
            abilities,
            vec![
                BotAbility::RequestPermission,
                BotAbility::SessionNotification,
                BotAbility::FsReadTextFile,
                BotAbility::FsWriteTextFile,
                BotAbility::TerminalCreate,
                BotAbility::TerminalOutput,
                BotAbility::TerminalRelease,
                BotAbility::TerminalWaitForExit,
                BotAbility::TerminalKill
            ]
        );
    }
}
