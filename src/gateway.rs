use crate::{
    backend::ComputerUseBackend,
    mutation_authority::MutationAuthorityError,
    policy::{PolicyDecision, ToolClass, ToolPolicy},
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
use std::{
    sync::{Arc, RwLock},
    time::Instant,
};
use tokio::sync::watch;
use tracing::{info, warn};

#[derive(Clone)]
pub struct Gateway {
    backend: Arc<dyn ComputerUseBackend>,
    policy: ToolPolicy,
    tools: Arc<RwLock<Vec<Tool>>>,
}

impl Gateway {
    pub async fn discover(
        backend: Arc<dyn ComputerUseBackend>,
        policy: ToolPolicy,
    ) -> Result<Self> {
        let gateway = Self {
            backend,
            policy,
            tools: Arc::new(RwLock::new(Vec::new())),
        };
        gateway
            .refresh_tools()
            .await
            .context("failed to discover backend MCP tools")?;
        Ok(gateway)
    }

    async fn refresh_tools(&self) -> Result<Vec<Tool>> {
        let discovered = self
            .backend
            .list_tools()
            .await
            .context("failed to discover backend MCP tools")?;
        let total = discovered.len();
        let tools: Vec<Tool> = discovered
            .into_iter()
            .filter(|tool| self.policy.evaluate(tool.name.as_ref()) == PolicyDecision::Allow)
            .collect();

        let mut observe = 0usize;
        let mut interact = 0usize;
        let mut system = 0usize;
        let mut dangerous = 0usize;
        for tool in &tools {
            match self.policy.classify(tool.name.as_ref()) {
                ToolClass::Observe => observe += 1,
                ToolClass::Interact => interact += 1,
                ToolClass::System => system += 1,
                ToolClass::Dangerous => dangerous += 1,
            }
        }

        {
            let mut cached = self
                .tools
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *cached = tools.clone();
        }

        info!(
            event = "backend_tool_discovery",
            discovered_tools = total,
            exposed_tools = tools.len(),
            observe_tools = observe,
            interact_tools = interact,
            system_tools = system,
            dangerous_tools = dangerous,
            "backend MCP tool discovery complete"
        );
        Ok(tools)
    }

    fn cached_tools(&self) -> Vec<Tool> {
        self.tools
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn exposed_tool(&self, name: &str) -> Option<Tool> {
        self.tools
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
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
        let tools = match self.refresh_tools().await {
            Ok(tools) => tools,
            Err(_) => {
                warn!(
                    event = "backend_tool_discovery",
                    outcome = "cached_fallback",
                    "backend tool refresh failed; serving the last policy-filtered snapshot"
                );
                self.cached_tools()
            }
        };

        Ok(ListToolsResult {
            tools,
            ..Default::default()
        })
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.exposed_tool(name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let started = Instant::now();
        let tool_name = request.name.to_string();
        let tool_class = self.policy.classify(&tool_name).as_str();

        if self.policy.evaluate(&tool_name) == PolicyDecision::Deny {
            warn!(
                event = "mcp_tool_call",
                tool = %tool_name,
                tool_class,
                policy = "deny",
                outcome = "blocked",
                duration_ms = started.elapsed().as_millis() as u64,
                "tool call blocked by gateway policy"
            );
            return Ok(Self::blocked_result());
        }

        // Refresh once when a policy-allowed tool is not in the current snapshot.
        // This lets backend upgrades become visible without restarting the gateway,
        // while still failing closed if discovery cannot confirm the tool exists.
        if self.exposed_tool(&tool_name).is_none() {
            let _ = self.refresh_tools().await;
            if self.exposed_tool(&tool_name).is_none() {
                warn!(
                    event = "mcp_tool_call",
                    tool = %tool_name,
                    tool_class,
                    policy = "allow",
                    outcome = "unavailable",
                    duration_ms = started.elapsed().as_millis() as u64,
                    "tool is allowed by policy but unavailable in the backend snapshot"
                );
                return Ok(Self::blocked_result());
            }
        }

        // rmcp cancels RequestContext::ct when the upstream client sends
        // notifications/cancelled. Forward that signal into the backend call;
        // CuaBackend then emits a cancellation notification with the actual
        // downstream request ID instead of merely dropping the local future.
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let upstream_ct = context.ct.clone();
        let cancellation_forwarder = tokio::spawn(async move {
            upstream_ct.cancelled().await;
            let _ = cancel_tx.send(true);
        });

        let backend_result = self
            .backend
            .call_tool(&tool_name, request.arguments, cancel_rx)
            .await;
        cancellation_forwarder.abort();

        match backend_result {
            Ok(result) => {
                info!(
                    event = "mcp_tool_call",
                    tool = %tool_name,
                    tool_class,
                    policy = "allow",
                    outcome = if result.is_error.unwrap_or(false) { "tool_error" } else { "success" },
                    duration_ms = started.elapsed().as_millis() as u64,
                    "tool call completed"
                );
                Ok(result.into())
            }
            Err(error) if error.downcast_ref::<MutationAuthorityError>().is_some() => {
                let authority = error
                    .downcast_ref::<MutationAuthorityError>()
                    .expect("guarded mutation authority error");
                warn!(
                    event = "mcp_tool_call",
                    tool = %tool_name,
                    tool_class,
                    policy = "allow",
                    outcome = "mutation_authority_refused",
                    error_code = authority.safe_code(),
                    duration_ms = started.elapsed().as_millis() as u64,
                    "effectful tool call refused before backend dispatch by shared mutation authority"
                );
                Ok(CallToolResult::error(vec![ContentBlock::text(
                    "This effectful tool call was refused before backend dispatch because this gateway does not currently hold the shared desktop mutation authority."
                )])
                .into())
            }
            Err(_) if context.ct.is_cancelled() => {
                warn!(
                    event = "mcp_tool_call",
                    tool = %tool_name,
                    tool_class,
                    policy = "allow",
                    outcome = "cancelled",
                    duration_ms = started.elapsed().as_millis() as u64,
                    "tool call cancelled; downstream cancellation was propagated"
                );
                Ok(CallToolResult::error(vec![ContentBlock::text(
                    "The computer-use tool call was cancelled. Cancellation was propagated to the backend and the call was not replayed."
                )])
                .into())
            }
            Err(_) => {
                warn!(
                    event = "mcp_tool_call",
                    tool = %tool_name,
                    tool_class,
                    policy = "allow",
                    outcome = "backend_error",
                    duration_ms = started.elapsed().as_millis() as u64,
                    "backend tool call failed"
                );
                Ok(CallToolResult::error(vec![ContentBlock::text(
                    "The computer-use backend could not complete this tool call. Its connection is recovered for a subsequent request when possible; the failed call is never replayed automatically."
                )])
                .into())
            }
        }
    }
}
