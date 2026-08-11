//! Cua Driver MCP backend.
//!
//! V1 deliberately treats Cua as an MCP server and forwards its tool surface
//! rather than reimplementing desktop automation in the gateway.
//!
//! On macOS, `cua-driver mcp` may proxy through the supported CuaDriver.app
//! daemon. Do not replace that lifecycle with a raw `cua-driver serve` spawn.

use super::{BackendHealth, ComputerUseBackend};
use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use rmcp::{
    model::{
        CallToolRequest, CallToolRequestParams, CallToolResult, CancelledNotificationParam,
        ClientRequest, JsonObject, ServerResult, Tool,
    },
    service::{Peer, PeerRequestOptions, RoleClient, RunningService, ServiceExt},
    transport::TokioChildProcess,
};
use std::{sync::Arc, time::Duration};
use tokio::{
    process::Command,
    sync::{Mutex, watch},
    time::{sleep, timeout},
};
use tracing::warn;

#[derive(Clone)]
pub struct CuaBackend {
    command: String,
    args: Vec<String>,
    connect_timeout: Duration,
    tool_timeout: Duration,
    reconnect_attempts: u32,
    reconnect_backoff: Duration,
    service: Arc<Mutex<Option<RunningService<RoleClient, ()>>>>,
    reconnect_lock: Arc<Mutex<()>>,
    operation_lock: Arc<Mutex<()>>,
}

impl std::fmt::Debug for CuaBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CuaBackend")
            .field("command", &self.command)
            .field("arg_count", &self.args.len())
            .field("connect_timeout", &self.connect_timeout)
            .field("tool_timeout", &self.tool_timeout)
            .field("reconnect_attempts", &self.reconnect_attempts)
            .finish_non_exhaustive()
    }
}

impl CuaBackend {
    pub fn new(
        command: impl Into<String>,
        args: Vec<String>,
        connect_timeout: Duration,
        tool_timeout: Duration,
        reconnect_attempts: u32,
        reconnect_backoff: Duration,
    ) -> Self {
        Self {
            command: command.into(),
            args,
            connect_timeout,
            tool_timeout,
            reconnect_attempts: reconnect_attempts.max(1),
            reconnect_backoff,
            service: Arc::new(Mutex::new(None)),
            reconnect_lock: Arc::new(Mutex::new(())),
            operation_lock: Arc::new(Mutex::new(())),
        }
    }

    async fn is_connected(&self) -> bool {
        let slot = self.service.lock().await;
        slot.as_ref()
            .is_some_and(|service| !service.is_closed() && !service.peer().is_transport_closed())
    }

    async fn close_current(&self) {
        let service = self.service.lock().await.take();
        if let Some(mut service) = service {
            let _ = timeout(self.connect_timeout, service.close()).await;
        }
    }

    async fn connect_once(&self) -> Result<()> {
        if self.is_connected().await {
            return Ok(());
        }

        self.close_current().await;

        let mut command = Command::new(&self.command);
        command.args(&self.args);
        command.kill_on_drop(true);

        let transport =
            TokioChildProcess::new(command).context("failed to spawn Cua MCP backend process")?;
        let service = timeout(self.connect_timeout, ().serve(transport))
            .await
            .context("timed out initializing Cua MCP backend")?
            .context("failed to initialize Cua MCP backend")?;

        *self.service.lock().await = Some(service);
        Ok(())
    }

    async fn connect_with_backoff(&self) -> Result<()> {
        let _reconnect = self.reconnect_lock.lock().await;
        if self.is_connected().await {
            return Ok(());
        }

        let mut last_error = None;
        for attempt in 0..self.reconnect_attempts {
            match self.connect_once().await {
                Ok(()) => return Ok(()),
                Err(error) => {
                    last_error = Some(error);
                    if attempt + 1 < self.reconnect_attempts {
                        let factor = 1_u32.checked_shl(attempt.min(31)).unwrap_or(u32::MAX);
                        sleep(self.reconnect_backoff.saturating_mul(factor)).await;
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow!("Cua MCP backend connection failed")))
    }

    async fn peer(&self) -> Result<Peer<RoleClient>> {
        if !self.is_connected().await {
            self.connect_with_backoff().await?;
        }

        let service = self.service.lock().await;
        let service = service
            .as_ref()
            .context("Cua MCP backend is not connected")?;

        if service.is_closed() || service.peer().is_transport_closed() {
            bail!("Cua MCP backend transport is closed");
        }

        Ok(service.peer().clone())
    }

    async fn list_tools_once(&self) -> Result<Vec<Tool>> {
        let peer = self.peer().await?;
        timeout(self.tool_timeout, peer.list_all_tools())
            .await
            .context("timed out listing Cua MCP tools")?
            .context("failed to list Cua MCP tools")
    }

    async fn recover_after_failure(&self) {
        self.close_current().await;
        if let Err(error) = self.connect_with_backoff().await {
            warn!(
                event = "backend_reconnect",
                outcome = "failed",
                error = %error,
                "Cua MCP backend recovery failed"
            );
        }
    }

    async fn notify_cancelled(
        peer: &Peer<RoleClient>,
        request_id: rmcp::model::RequestId,
        reason: &str,
    ) {
        let _ = peer
            .notify_cancelled(CancelledNotificationParam::new(
                Some(request_id),
                Some(reason.to_owned()),
            ))
            .await;
    }
}

#[async_trait]
impl ComputerUseBackend for CuaBackend {
    async fn connect(&self) -> Result<()> {
        self.connect_with_backoff().await
    }

    async fn health(&self) -> BackendHealth {
        let slot = self.service.lock().await;
        match slot.as_ref() {
            None => BackendHealth::Stopped,
            Some(service) if service.is_closed() || service.peer().is_transport_closed() => {
                BackendHealth::Unhealthy
            }
            Some(_) => BackendHealth::Ready,
        }
    }

    async fn list_tools(&self) -> Result<Vec<Tool>> {
        let _operation = self.operation_lock.lock().await;
        match self.list_tools_once().await {
            Ok(tools) => Ok(tools),
            Err(first_error) => {
                self.recover_after_failure().await;
                self.list_tools_once().await.with_context(|| {
                    format!("Cua MCP tool discovery failed after reconnect: {first_error:#}")
                })
            }
        }
    }

    async fn call_tool(
        &self,
        name: &str,
        arguments: Option<JsonObject>,
        mut cancellation: watch::Receiver<bool>,
    ) -> Result<CallToolResult> {
        // A single physical desktop is a shared mutable resource. Serialize all
        // backend operations in V1 so independent MCP clients cannot interleave
        // clicks, keystrokes, snapshots, and stateful element-index operations.
        let _operation = self.operation_lock.lock().await;
        let peer = self.peer().await?;
        let mut params = CallToolRequestParams::new(name.to_owned());
        if let Some(arguments) = arguments {
            params = params.with_arguments(arguments);
        }

        let handle = peer
            .send_cancellable_request(
                ClientRequest::CallToolRequest(CallToolRequest::new(params)),
                PeerRequestOptions::no_options(),
            )
            .await
            .context("failed to send Cua MCP tool call")?;
        let request_id = handle.id.clone();
        let response = handle.await_response();
        tokio::pin!(response);

        tokio::select! {
            result = &mut response => {
                match result {
                    Ok(ServerResult::CallToolResult(result)) => Ok(result),
                    Ok(_) => bail!("Cua MCP tool call returned an unsupported multi-round-trip response"),
                    Err(error) => {
                        self.recover_after_failure().await;
                        Err(error).context("Cua MCP tool call failed; connection recovered for the next call")
                    }
                }
            }
            changed = cancellation.changed() => {
                if changed.is_ok() && *cancellation.borrow() {
                    Self::notify_cancelled(&peer, request_id, "upstream MCP request cancelled").await;
                    bail!("Cua MCP tool call cancelled by the upstream MCP client; the call was not replayed")
                }
                bail!("Cua MCP cancellation channel closed unexpectedly")
            }
            _ = sleep(self.tool_timeout) => {
                Self::notify_cancelled(&peer, request_id, "gateway tool timeout").await;
                self.recover_after_failure().await;
                bail!(
                    "Cua MCP tool call timed out after {} seconds; cancellation was propagated and the call was not retried because its side effects may be unknown",
                    self.tool_timeout.as_secs()
                )
            }
        }
    }

    async fn shutdown(&self) -> Result<()> {
        let _reconnect = self.reconnect_lock.lock().await;
        self.close_current().await;
        Ok(())
    }
}
