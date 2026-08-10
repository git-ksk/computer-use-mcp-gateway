//! Cua Driver backend boundary.
//!
//! The real MCP adapter is intentionally isolated here. V1 will connect to
//! `cua-driver mcp` through rmcp's child-process transport and translate the
//! returned MCP Tool/CallToolResult models into the backend-neutral types.
//!
//! On macOS, do not bypass Cua's documented TCC/process-identity lifecycle.

use super::{BackendHealth, BackendTool, ComputerUseBackend};
use anyhow::{Result, bail};
use async_trait::async_trait;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct CuaBackend {
    command: String,
    args: Vec<String>,
}

impl CuaBackend {
    pub fn new(command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            command: command.into(),
            args,
        }
    }

    pub fn command(&self) -> &str {
        &self.command
    }

    pub fn args(&self) -> &[String] {
        &self.args
    }
}

#[async_trait]
impl ComputerUseBackend for CuaBackend {
    async fn health(&self) -> BackendHealth {
        BackendHealth::Starting
    }

    async fn list_tools(&self) -> Result<Vec<BackendTool>> {
        bail!("Cua MCP adapter not wired yet; roadmap milestone M1")
    }

    async fn call_tool(&self, _name: &str, _arguments: Option<Value>) -> Result<Value> {
        bail!("Cua MCP adapter not wired yet; roadmap milestone M1")
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}
