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
