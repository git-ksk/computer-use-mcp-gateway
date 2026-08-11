pub mod cua;

use anyhow::Result;
use async_trait::async_trait;
use rmcp::model::{CallToolResult, JsonObject, Tool};
use tokio::sync::watch;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendHealth {
    Ready,
    Unhealthy,
    Stopped,
}

#[async_trait]
pub trait ComputerUseBackend: Send + Sync {
    async fn connect(&self) -> Result<()>;
    async fn health(&self) -> BackendHealth;
    async fn list_tools(&self) -> Result<Vec<Tool>>;
    async fn call_tool(
        &self,
        name: &str,
        arguments: Option<JsonObject>,
        cancellation: watch::Receiver<bool>,
    ) -> Result<CallToolResult>;
    async fn shutdown(&self) -> Result<()>;
}
