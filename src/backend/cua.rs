//! Cua Driver MCP backend.
//!
//! V1 deliberately treats Cua as an MCP server and forwards its tool surface
//! rather than reimplementing desktop automation in the gateway.
//!
//! On macOS, `cua-driver mcp` may proxy through the supported CuaDriver.app
//! daemon. Do not replace that lifecycle with a raw `cua-driver serve` spawn.

use super::{BackendHealth, ComputerUseBackend};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use rmcp::{
    model::{CallToolRequestParams, CallToolResult, JsonObject, Tool},
    service::{RoleClient, RunningService, ServiceExt},
    transport::TokioChildProcess,
};
use std::sync::Arc;
use tokio::{process::Command, sync::Mutex};

#[derive(Clone)]
pub struct CuaBackend {
    command: String,
    args: Vec<String>,
    service: Arc<Mutex<Option<RunningService<RoleClient, ()>>>>,
}

impl std::fmt::Debug for CuaBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CuaBackend")
            .field("command", &self.command)
            .field("args", &self.args)
            .finish_non_exhaustive()
    }
}

impl CuaBackend {
    pub fn new(command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            command: command.into(),
            args,
            service: Arc::new(Mutex::new(None)),
        }
    }

    pub fn command(&self) -> &str {
        &self.command
    }

    pub fn args(&self) -> &[String] {
        &self.args
    }

    async fn peer(&self) -> Result<rmcp::service::Peer<RoleClient>> {
        let service = self.service.lock().await;
        let service = service
            .as_ref()
            .context("Cua MCP backend is not connected")?;

        if service.is_closed() || service.peer().is_transport_closed() {
            bail!("Cua MCP backend transport is closed");
        }

        Ok(service.peer().clone())
    }
}

#[async_trait]
impl ComputerUseBackend for CuaBackend {
    async fn connect(&self) -> Result<()> {
        let mut slot = self.service.lock().await;
        if let Some(service) = slot.as_ref()
            && !service.is_closed()
            && !service.peer().is_transport_closed()
        {
            return Ok(());
        }

        let mut command = Command::new(&self.command);
        command.args(&self.args);
        command.kill_on_drop(true);

        let transport =
            TokioChildProcess::new(command).context("failed to spawn Cua MCP backend process")?;
        let service = ().serve(transport).await.context("failed to initialize Cua MCP backend")?;

        *slot = Some(service);
        Ok(())
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
        let peer = self.peer().await?;
        peer.list_all_tools()
            .await
            .context("failed to list Cua MCP tools")
    }

    async fn call_tool(&self, name: &str, arguments: Option<JsonObject>) -> Result<CallToolResult> {
        let peer = self.peer().await?;
        let mut request = CallToolRequestParams::new(name.to_owned());
        if let Some(arguments) = arguments {
            request = request.with_arguments(arguments);
        }

        peer.call_tool(request)
            .await
            .context("Cua MCP tool call failed")
    }

    async fn shutdown(&self) -> Result<()> {
        let service = self.service.lock().await.take();
        if let Some(mut service) = service {
            service
                .close()
                .await
                .context("failed to join Cua MCP service during shutdown")?;
        }
        Ok(())
    }
}
