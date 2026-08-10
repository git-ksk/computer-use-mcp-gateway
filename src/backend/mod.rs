pub mod cua;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendHealth {
    Starting,
    Ready,
    Unhealthy(String),
    Stopped,
}

#[derive(Debug, Clone)]
pub struct BackendTool {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
}

#[async_trait]
pub trait ComputerUseBackend: Send + Sync {
    async fn health(&self) -> BackendHealth;
    async fn list_tools(&self) -> Result<Vec<BackendTool>>;
    async fn call_tool(&self, name: &str, arguments: Option<Value>) -> Result<Value>;
    async fn shutdown(&self) -> Result<()>;
}
