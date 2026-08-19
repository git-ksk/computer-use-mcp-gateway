from pathlib import Path
import re


def read(path: str) -> str:
    return Path(path).read_text()


def write(path: str, text: str) -> None:
    Path(path).write_text(text)


def must_replace(text: str, old: str, new: str, label: str) -> str:
    if old not in text:
        raise SystemExit(f"missing expected block: {label}")
    return text.replace(old, new, 1)


# lib.rs: remove the module entirely.
p = "src/lib.rs"
s = read(p)
s = s.replace("\npub mod v2_usage;\n", "\n")
write(p, s)

# Hub: execution safety stays authoritative; remove accounting state/dispatch hooks.
p = "src/v2_m1_hub.rs"
s = read(p)
s = s.replace("use crate::v2_usage::UsageLease;\n", "")
s = re.sub(r"(?m)^\s*usage: Option<UsageLease>,\n", "", s)
s = re.sub(r"(?m)^\s*usage,\n", "", s)
s = must_replace(
    s,
    """        let (owner, device_command, usage) = {
            let operation = pending
                .get(operation_id)
                .ok_or(HubServiceError::PendingOperationMissing)?;
            (
                operation.owner.clone(),
                operation.command.clone(),
                operation.usage.clone(),
            )
        };""",
    """        let (owner, device_command) = {
            let operation = pending
                .get(operation_id)
                .ok_or(HubServiceError::PendingOperationMissing)?;
            (operation.owner.clone(), operation.command.clone())
        };""",
    "hub dispatch tuple",
)
s = re.sub(
    r"\n        // Usage accounting is deliberately outside the authoritative execution.*?\n        // A shutdown signal may arrive while usage accounting is awaiting its\n        // pre-dispatch liability transition\. Re-check the drain gate before the\n        // durable side-effect boundary so such work remains provably unexecuted\.\n",
    "\n        // Re-check the drain gate immediately before the durable side-effect boundary.\n",
    s,
    count=1,
    flags=re.S,
)
s = s.replace(
    "        if let Some(usage) = usage.as_ref() {\n            usage.mark_dispatched();\n        }\n",
    "",
)
s = s.replace(
    "self.start_command_as_with_id(owner, random_operation_id(), command, None)",
    "self.start_command_as_with_id(owner, random_operation_id(), command)",
)
s = s.replace(
    "    /// Allocates the CUMG logical operation identity before accounting admission.\n"
    "    /// The same value is passed unchanged to MCPUsage as `operationId`.\n",
    "    /// Allocates a CUMG logical operation identity before command admission.\n",
)
s = re.sub(
    r"        if operation_id\.is_empty\(\)\n            \|\| usage\n                \.as_ref\(\)\n                \.is_some_and\(\|lease\| lease\.operation_id\(\) != operation_id\)\n        \{",
    "        if operation_id.is_empty() {",
    s,
    count=1,
)
s = s.replace("    UsageUnavailable,\n", "")
s = s.replace('            Self::UsageUnavailable => "usage_unavailable",\n', "")
marker = "    #[test]\n    fn usage_liability_boundary_precedes_cumg_dispatched_and_agent_send() {"
if marker in s:
    s = s[: s.index(marker)] + "}\n"
write(p, s)

# Northbound: keep authentication/cancellation/audit; remove quota lifecycle.
p = "src/v2_m1_northbound.rs"
s = read(p)
s = s.replace(
    "    v2_usage::{UsageError, UsageLease, UsageManager, UsageOperation, UsageSettlement},\n",
    "",
)
s = s.replace(
    "use tokio::{sync::Mutex as TokioMutex, time::MissedTickBehavior};",
    "use tokio::sync::Mutex as TokioMutex;",
)
old = """    pub fn new(hub: HubHandle, policy: ClientAuthorizationPolicy) -> Self {
        Self::new_with_authorizer_and_usage(hub, Arc::new(policy), UsageManager::noop())
    }

    pub fn new_with_usage(
        hub: HubHandle,
        policy: ClientAuthorizationPolicy,
        usage: UsageManager,
    ) -> Self {
        Self::new_with_authorizer_and_usage(hub, Arc::new(policy), usage)
    }

    pub fn new_with_authorizer(
        hub: HubHandle,
        authorizer: Arc<dyn DeviceCapabilityAuthorizer>,
    ) -> Self {
        Self::new_with_authorizer_and_usage(hub, authorizer, UsageManager::noop())
    }

    pub fn new_with_authorizer_and_usage(
        hub: HubHandle,
        authorizer: Arc<dyn DeviceCapabilityAuthorizer>,
        usage: UsageManager,
    ) -> Self {
        Self {
            hub,
            authorizer,
            usage,
            request_fingerprint_secret: None,
            interactions: Arc::new(TokioMutex::new(NorthboundInteractionState::new())),
        }
    }
"""
new = """    pub fn new(hub: HubHandle, policy: ClientAuthorizationPolicy) -> Self {
        Self::new_with_authorizer(hub, Arc::new(policy))
    }

    pub fn new_with_authorizer(
        hub: HubHandle,
        authorizer: Arc<dyn DeviceCapabilityAuthorizer>,
    ) -> Self {
        Self {
            hub,
            authorizer,
            request_fingerprint_secret: None,
            interactions: Arc::new(TokioMutex::new(NorthboundInteractionState::new())),
        }
    }
"""
s = must_replace(s, old, new, "northbound constructors")
s = s.replace("    usage: UsageLease,\n", "")
s = s.replace("    usage: UsageManager,\n", "")

# Browser-stage/browser-tool validation failures no longer have accounting cleanup.
s = re.sub(
    r"Err\(error\) => \{\n\s*settle_usage_best_effort\(\n\s*&operation\.usage,\n\s*UsageSettlement::Zero,\n\s*\"[^\"]+\",\n\s*\)\n\s*\.await;\n\s*return Err\((.*?)\);\n\s*\}",
    r"Err(error) => return Err(\1)",
    s,
)

# Replace command execution with the CUMG-native lifecycle only.
pat = re.compile(r"    async fn execute_command\(\n.*?\n    \}\n\}\n\nimpl ServerHandler", re.S)
repl = """    async fn execute_command(
        &self,
        principal: &AuthenticatedClientPrincipal,
        command: DeviceCommand,
        operation: NorthboundOperationCall,
        context: &RequestContext<RoleServer>,
    ) -> Result<DeviceResult, McpError> {
        let request_fingerprint = self.request_fingerprint_for_command(&command)?;
        let NorthboundOperationCall { operation_id, audit } = operation;
        let owner = OperationOwner::from_principal(principal);
        let pending = self
            .hub
            .start_command_as_with_id_and_audit(
                owner.clone(),
                operation_id.clone(),
                command,
                audit,
                request_fingerprint,
            )
            .await
            .map_err(hub_error_to_mcp)?;
        let mut wait = Box::pin(pending.wait());
        tokio::select! {
            result = &mut wait => result.map(|result| result.result).map_err(hub_error_to_mcp),
            _ = context.ct.cancelled() => {
                let cancellation = self.hub.cancel_as(owner, operation_id).await;
                if let Err(error) = cancellation {
                    warn!(
                        event = "v2_northbound_cancel_failed",
                        outcome = "original_call_cancelled",
                        error_code = error.safe_error_code(),
                        "CUMG cancellation request failed; execution safety state remains authoritative"
                    );
                }
                Err(McpError::invalid_request("Tool call was cancelled", None))
            }
        }
    }
}

impl ServerHandler"""
s, n = pat.subn(repl, s, count=1)
if n != 1:
    raise SystemExit("failed to replace execute_command")

# Remove reserve/admission and accounting-only early-error cleanup from call_tool.
s = re.sub(
    r"        // OAuth has already reduced the bearer token.*?\.map_err\(usage_error_to_mcp\)\?;\n\n",
    "",
    s,
    count=1,
    flags=re.S,
)
s = re.sub(
    r"        if let Err\(error\) = self\.authorize\(&auth\.principal, capability\) \{\n\s*settle_usage_best_effort.*?\n\s*return Err\(error\);\n\s*\}\n",
    "        self.authorize(&auth.principal, capability)?;\n",
    s,
    count=1,
    flags=re.S,
)
s = re.sub(r"(?m)^\s*usage,\n", "", s)
s = re.sub(
    r"        let command = match command_result \{\n            Ok\(command\) => command,\n            Err\(error\) => \{\n                settle_usage_best_effort.*?\n                return Err\(error\);\n            \}\n        \};",
    "        let command = command_result?;",
    s,
    count=1,
    flags=re.S,
)
s = re.sub(
    r"        let \(command, interaction_binding\) = match self\n            \.prepare_contextual_command\(&auth\.principal, command\)\n            \.await\n        \{\n            Ok\(prepared\) => prepared,\n            Err\(error\) => \{\n                settle_usage_best_effort\(.*?\n                return Err\(error\);\n            \}\n        \};",
    "        let (command, interaction_binding) = self\n            .prepare_contextual_command(&auth.principal, command)\n            .await?;",
    s,
    count=1,
    flags=re.S,
)

# Remove accounting helper functions and usage-specific Hub error mapping.
s = re.sub(
    r"\nfn usage_settlement_for_error\(.*?\nfn hub_error_to_mcp",
    "\nfn hub_error_to_mcp",
    s,
    count=1,
    flags=re.S,
)
s = re.sub(
    r"        HubCommandError::UsageUnavailable => \(\n"
    r"            \"Usage accounting is temporarily unavailable\",\n"
    r"            \"usage_unavailable\",\n"
    r"            None,\n"
    r"        \),\n",
    "",
    s,
    count=1,
)
marker = "    #[test]\n    fn screenshot_failure_is_read_only_but_type_text_is_mutating_for_accounting() {"
if marker in s:
    s = s[: s.index(marker)] + "}\n"
write(p, s)

# Hub binary: remove usage endpoint/timeout and controller construction.
p = "src/bin/v2_hub.rs"
s = read(p)
s = s.replace("    v2_usage::{McpUsageController, UsageManager},\n", "")
s = re.sub(
    r"    /// Optional loopback-only mcp-usage-control sidecar endpoint\. When omitted,\n"
    r"    /// V2 uses the no-op accounting controller and preserves pre-integration behavior\.\n"
    r"    #\[arg\(long, env = \"CUMG_V2_USAGE_ENDPOINT\"\)\]\n"
    r"    usage_endpoint: Option<String>,\n"
    r"    #\[arg\(long, env = \"CUMG_V2_USAGE_TIMEOUT_SECS\", default_value_t = 2\)\]\n"
    r"    usage_timeout_secs: u64,\n",
    "",
    s,
    count=1,
)
s = re.sub(
    r"    ensure!\(\n"
    r"        args\.usage_timeout_secs > 0,\n"
    r"        \"CUMG_V2_USAGE_TIMEOUT_SECS must be greater than zero\"\n"
    r"    \);\n",
    "",
    s,
    count=1,
)
s = re.sub(r"\n        usage_accounting_enabled = args\.usage_endpoint\.is_some\(\),", "", s, count=1)
s = s.replace("        args.usage_endpoint.is_some(),\n", "")
s = re.sub(
    r"    let usage = if let Some\(endpoint\) = args\.usage_endpoint\.as_deref\(\) \{.*?    \};\n",
    "",
    s,
    count=1,
    flags=re.S,
)
s = s.replace(
    "V2NorthboundMcp::new_with_usage(handle, policy, usage)",
    "V2NorthboundMcp::new(handle, policy)",
)
write(p, s)

Path("src/v2_usage.rs").unlink()
