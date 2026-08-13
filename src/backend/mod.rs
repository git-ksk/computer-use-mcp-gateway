pub mod cua;

use anyhow::Result;
use async_trait::async_trait;
use rmcp::model::{CallToolResult, JsonObject, Tool};
use serde::Serialize;
use tokio::sync::watch;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendHealth {
    Ready,
    Unhealthy,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BackendResourceMetrics {
    pub pid: u32,
    pub cpu_seconds: Option<f64>,
    pub rss_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendCallCancelled;

impl std::fmt::Display for BackendCallCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Cua MCP tool call cancelled by the upstream MCP client; cancellation was propagated and the call was not replayed"
        )
    }
}

impl std::error::Error for BackendCallCancelled {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendCallTimedOut {
    pub timeout_secs: u64,
}

impl std::fmt::Display for BackendCallTimedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Cua MCP tool call timed out after {} seconds; cancellation was propagated and the call was not retried because its side effects may be unknown",
            self.timeout_secs
        )
    }
}

impl std::error::Error for BackendCallTimedOut {}

#[async_trait]
pub trait ComputerUseBackend: Send + Sync {
    async fn connect(&self) -> Result<()>;
    async fn health(&self) -> BackendHealth;
    async fn resource_metrics(&self) -> Option<BackendResourceMetrics>;
    async fn list_tools(&self) -> Result<Vec<Tool>>;
    async fn call_tool(
        &self,
        name: &str,
        arguments: Option<JsonObject>,
        cancellation: watch::Receiver<bool>,
    ) -> Result<CallToolResult>;
    async fn shutdown(&self) -> Result<()>;
}
