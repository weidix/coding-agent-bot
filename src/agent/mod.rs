use agent_client_protocol as acp;
use async_trait::async_trait;

/// Unified agent interface for ACP agent-side responsibilities.
///
/// Method signatures intentionally reuse ACP schema types directly.
#[async_trait(?Send)]
pub trait Agent {
    /// Establishes the connection and negotiates protocol capabilities.
    async fn initialize(
        &self,
        args: acp::InitializeRequest,
    ) -> acp::Result<acp::InitializeResponse>;

    /// Authenticates the client with an advertised authentication method.
    async fn authenticate(
        &self,
        args: acp::AuthenticateRequest,
    ) -> acp::Result<acp::AuthenticateResponse>;

    /// Creates a new session.
    async fn new_session(
        &self,
        args: acp::NewSessionRequest,
    ) -> acp::Result<acp::NewSessionResponse>;

    /// Handles a prompt turn for a session.
    async fn prompt(&self, args: acp::PromptRequest) -> acp::Result<acp::PromptResponse>;

    /// Cancels ongoing work for a session.
    async fn cancel(&self, args: acp::CancelNotification) -> acp::Result<()>;

    /// Optional: `session/load`.
    async fn load_session(
        &self,
        _args: acp::LoadSessionRequest,
    ) -> acp::Result<acp::LoadSessionResponse> {
        Err(acp::Error::method_not_found())
    }

    /// Optional: `session/set_mode`.
    async fn set_session_mode(
        &self,
        _args: acp::SetSessionModeRequest,
    ) -> acp::Result<acp::SetSessionModeResponse> {
        Err(acp::Error::method_not_found())
    }

    /// Optional: `session/set_config_option`.
    async fn set_session_config_option(
        &self,
        _args: acp::SetSessionConfigOptionRequest,
    ) -> acp::Result<acp::SetSessionConfigOptionResponse> {
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

/// Adapter that exposes an [`Agent`] implementation as ACP [`acp::Agent`].
#[derive(Debug, Clone)]
pub struct AcpAgentAdapter<A> {
    inner: A,
}

impl<A> AcpAgentAdapter<A> {
    #[must_use]
    pub fn new(inner: A) -> Self {
        Self { inner }
    }

    #[must_use]
    pub fn into_inner(self) -> A {
        self.inner
    }
}

#[async_trait(?Send)]
impl<A: Agent> acp::Agent for AcpAgentAdapter<A> {
    async fn initialize(
        &self,
        args: acp::InitializeRequest,
    ) -> acp::Result<acp::InitializeResponse> {
        self.inner.initialize(args).await
    }

    async fn authenticate(
        &self,
        args: acp::AuthenticateRequest,
    ) -> acp::Result<acp::AuthenticateResponse> {
        self.inner.authenticate(args).await
    }

    async fn new_session(
        &self,
        args: acp::NewSessionRequest,
    ) -> acp::Result<acp::NewSessionResponse> {
        self.inner.new_session(args).await
    }

    async fn prompt(&self, args: acp::PromptRequest) -> acp::Result<acp::PromptResponse> {
        self.inner.prompt(args).await
    }

    async fn cancel(&self, args: acp::CancelNotification) -> acp::Result<()> {
        self.inner.cancel(args).await
    }

    async fn load_session(
        &self,
        args: acp::LoadSessionRequest,
    ) -> acp::Result<acp::LoadSessionResponse> {
        self.inner.load_session(args).await
    }

    async fn set_session_mode(
        &self,
        args: acp::SetSessionModeRequest,
    ) -> acp::Result<acp::SetSessionModeResponse> {
        self.inner.set_session_mode(args).await
    }

    async fn set_session_config_option(
        &self,
        args: acp::SetSessionConfigOptionRequest,
    ) -> acp::Result<acp::SetSessionConfigOptionResponse> {
        self.inner.set_session_config_option(args).await
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
    use std::cell::Cell;

    struct NoopAgent;

    #[async_trait(?Send)]
    impl Agent for NoopAgent {
        async fn initialize(
            &self,
            _args: acp::InitializeRequest,
        ) -> acp::Result<acp::InitializeResponse> {
            Err(acp::Error::method_not_found())
        }

        async fn authenticate(
            &self,
            _args: acp::AuthenticateRequest,
        ) -> acp::Result<acp::AuthenticateResponse> {
            Err(acp::Error::method_not_found())
        }

        async fn new_session(
            &self,
            _args: acp::NewSessionRequest,
        ) -> acp::Result<acp::NewSessionResponse> {
            Err(acp::Error::method_not_found())
        }

        async fn prompt(&self, _args: acp::PromptRequest) -> acp::Result<acp::PromptResponse> {
            Err(acp::Error::method_not_found())
        }

        async fn cancel(&self, _args: acp::CancelNotification) -> acp::Result<()> {
            Err(acp::Error::method_not_found())
        }
    }

    struct CountingAgent {
        cancel_calls: Cell<usize>,
    }

    #[async_trait(?Send)]
    impl Agent for CountingAgent {
        async fn initialize(
            &self,
            _args: acp::InitializeRequest,
        ) -> acp::Result<acp::InitializeResponse> {
            Err(acp::Error::method_not_found())
        }

        async fn authenticate(
            &self,
            _args: acp::AuthenticateRequest,
        ) -> acp::Result<acp::AuthenticateResponse> {
            Err(acp::Error::method_not_found())
        }

        async fn new_session(
            &self,
            _args: acp::NewSessionRequest,
        ) -> acp::Result<acp::NewSessionResponse> {
            Err(acp::Error::method_not_found())
        }

        async fn prompt(&self, _args: acp::PromptRequest) -> acp::Result<acp::PromptResponse> {
            Err(acp::Error::method_not_found())
        }

        async fn cancel(&self, _args: acp::CancelNotification) -> acp::Result<()> {
            self.cancel_calls.set(self.cancel_calls.get() + 1);
            Ok(())
        }
    }

    #[tokio::test]
    async fn optional_load_session_defaults_to_method_not_found() {
        let agent = NoopAgent;
        let result =
            Agent::load_session(&agent, acp::LoadSessionRequest::new("session-1", ".")).await;

        let err = result.expect_err("load_session should default to method_not_found");
        assert_eq!(err.code, acp::ErrorCode::MethodNotFound);
    }

    #[tokio::test]
    async fn optional_ext_method_defaults_to_null_payload() {
        let agent = NoopAgent;
        let request = acp::ExtRequest::new("ext.test", acp::RawValue::NULL.to_owned().into());

        let result = Agent::ext_method(&agent, request)
            .await
            .expect("ext_method default should return null payload");
        assert_eq!(result.0.get(), "null");
    }

    #[tokio::test]
    async fn adapter_forwards_cancel_to_inner_agent() {
        let adapter = AcpAgentAdapter::new(CountingAgent {
            cancel_calls: Cell::new(0),
        });

        acp::Agent::cancel(&adapter, acp::CancelNotification::new("session-1"))
            .await
            .expect("cancel should be forwarded");
        let inner = adapter.into_inner();
        assert_eq!(inner.cancel_calls.get(), 1);
    }
}
