use crate::{
    backend::ComputerUseBackend,
    policy::{PolicyDecision, ToolPolicy},
};
use anyhow::{Context, Result};
use rmcp::{
    ErrorData as McpError, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::{RequestContext, RoleServer},
};
use std::{sync::Arc, time::Instant};
use tracing::{info, warn};

#[derive(Clone)]
pub struct Gateway {
    backend: Arc<dyn ComputerUseBackend>,
    policy: ToolPolicy,
    tools: Arc<Vec<Tool>>,
}

impl Gateway {
    pub async fn discover(
        backend: Arc<dyn ComputerUseBackend>,
        policy: ToolPolicy,
    ) -> Result<Self> {
        let discovered = backend
            .list_tools()
            .await
            .context("failed to discover backend MCP tools")?;
        let total = discovered.len();
        let tools: Vec<Tool> = discovered
            .into_iter()
            .filter(|tool| policy.evaluate(tool.name.as_ref()) == PolicyDecision::Allow)
            .collect();

        info!(
            event = "backend_tool_discovery",
            discovered_tools = total,
            exposed_tools = tools.len(),
            "backend MCP tool discovery complete"
        );

        Ok(Self {
            backend,
            policy,
            tools: Arc::new(tools),
        })
    }

    fn exposed_tool(&self, name: &str) -> Option<Tool> {
        self.tools
            .iter()
            .find(|tool| tool.name.as_ref() == name)
            .cloned()
    }

    fn blocked_result() -> CallToolResponse {
        CallToolResult::error(vec![ContentBlock::text(
            "Tool is unavailable through this gateway policy.",
        )])
        .into()
    }
}

impl ServerHandler for Gateway {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "Forwards the configured local computer-use backend through a policy-controlled MCP boundary."
                .into(),
        );
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult {
            tools: self.tools.as_ref().clone(),
            ..Default::default()
        })
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.exposed_tool(name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let started = Instant::now();
        let tool_name = request.name.to_string();

        if self.policy.evaluate(&tool_name) == PolicyDecision::Deny
            || self.exposed_tool(&tool_name).is_none()
        {
            warn!(
                event = "mcp_tool_call",
                tool = %tool_name,
                policy = "deny",
                outcome = "blocked",
                duration_ms = started.elapsed().as_millis() as u64,
                "tool call blocked by gateway policy"
            );
            return Ok(Self::blocked_result());
        }

        match self.backend.call_tool(&tool_name, request.arguments).await {
            Ok(result) => {
                info!(
                    event = "mcp_tool_call",
                    tool = %tool_name,
                    policy = "allow",
                    outcome = if result.is_error.unwrap_or(false) { "tool_error" } else { "success" },
                    duration_ms = started.elapsed().as_millis() as u64,
                    "tool call completed"
                );
                Ok(result.into())
            }
            Err(_) => {
                warn!(
                    event = "mcp_tool_call",
                    tool = %tool_name,
                    policy = "allow",
                    outcome = "backend_error",
                    duration_ms = started.elapsed().as_millis() as u64,
                    "backend tool call failed"
                );
                Ok(CallToolResult::error(vec![ContentBlock::text(
                    "The computer-use backend could not complete this tool call.",
                )])
                .into())
            }
        }
    }
}
