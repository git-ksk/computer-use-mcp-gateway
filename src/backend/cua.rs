//! Cua Driver MCP backend.
//!
//! V1 deliberately treats Cua as an MCP server and forwards its tool surface
//! rather than reimplementing desktop automation in the gateway.
//!
//! On macOS, `cua-driver mcp` may proxy through the supported CuaDriver.app
//! daemon. Do not replace that lifecycle with a raw `cua-driver serve` spawn.

use super::{
    BackendCallCancelled, BackendCallTimedOut, BackendHealth, BackendResourceMetrics,
    ComputerUseBackend,
};
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
    backend_pid: Arc<Mutex<Option<u32>>>,
    reconnect_lock: Arc<Mutex<()>>,
    operation_lock: Arc<Mutex<()>>,
}

impl std::fmt::Debug for CuaBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CuaBackend")
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
            backend_pid: Arc::new(Mutex::new(None)),
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
        *self.backend_pid.lock().await = None;
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
        let pid = transport.id();
        let service = timeout(self.connect_timeout, ().serve(transport))
            .await
            .context("timed out initializing Cua MCP backend")?
            .context("failed to initialize Cua MCP backend")?;

        *self.service.lock().await = Some(service);
        *self.backend_pid.lock().await = pid;
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
            let _ = error;
            warn!(
                event = "v2_backend_reconnect",
                backend = "cua",
                outcome = "failed",
                error_code = "backend_reconnect_failed",
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

    #[cfg(unix)]
    async fn query_process_metrics(pid: u32) -> Option<BackendResourceMetrics> {
        let pid_arg = pid.to_string();
        let output = Command::new("ps")
            .args(["-o", "time=", "-o", "rss=", "-p", &pid_arg])
            .output()
            .await
            .ok()?;
        if !output.status.success() {
            return None;
        }

        let stdout = String::from_utf8(output.stdout).ok()?;
        let mut fields = stdout.split_whitespace();
        let cpu_seconds = fields.next().and_then(parse_ps_cpu_time);
        let rss_bytes = fields
            .next()
            .and_then(|value| value.parse::<u64>().ok())
            .map(|kib| kib.saturating_mul(1024));

        Some(BackendResourceMetrics {
            pid,
            cpu_seconds,
            rss_bytes,
        })
    }

    #[cfg(windows)]
    async fn query_process_metrics(pid: u32) -> Option<BackendResourceMetrics> {
        let script = format!(
            "$p=Get-Process -Id {pid} -ErrorAction Stop; \
             $c=[System.Globalization.CultureInfo]::InvariantCulture; \
             $cpu=if ($null -eq $p.CPU) {{'0'}} else {{$p.CPU.ToString($c)}}; \
             Write-Output ($cpu + ' ' + $p.WorkingSet64.ToString($c))"
        );
        let output = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()
            .await
            .ok()?;
        if !output.status.success() {
            return None;
        }

        let stdout = String::from_utf8(output.stdout).ok()?;
        let mut fields = stdout.split_whitespace();
        let cpu_seconds = fields.next().and_then(|value| value.parse::<f64>().ok());
        let rss_bytes = fields.next().and_then(|value| value.parse::<u64>().ok());

        Some(BackendResourceMetrics {
            pid,
            cpu_seconds,
            rss_bytes,
        })
    }

    #[cfg(not(any(unix, windows)))]
    async fn query_process_metrics(pid: u32) -> Option<BackendResourceMetrics> {
        Some(BackendResourceMetrics {
            pid,
            cpu_seconds: None,
            rss_bytes: None,
        })
    }
}

#[cfg(unix)]
fn parse_ps_cpu_time(value: &str) -> Option<f64> {
    let (days, clock) = match value.split_once('-') {
        Some((days, clock)) => (days.parse::<f64>().ok()?, clock),
        None => (0.0, value),
    };
    let parts: Vec<&str> = clock.split(':').collect();
    let (hours, minutes, seconds): (f64, f64, f64) = match parts.as_slice() {
        [minutes, seconds] => (
            0.0,
            minutes.parse::<f64>().ok()?,
            seconds.parse::<f64>().ok()?,
        ),
        [hours, minutes, seconds] => (
            hours.parse::<f64>().ok()?,
            minutes.parse::<f64>().ok()?,
            seconds.parse::<f64>().ok()?,
        ),
        _ => return None,
    };
    Some(days * 86_400.0 + hours * 3_600.0 + minutes * 60.0 + seconds)
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

    async fn resource_metrics(&self) -> Option<BackendResourceMetrics> {
        let pid = (*self.backend_pid.lock().await)?;
        Self::query_process_metrics(pid).await
    }

    async fn list_tools(&self) -> Result<Vec<Tool>> {
        let _operation = self.operation_lock.lock().await;
        match self.list_tools_once().await {
            Ok(tools) => Ok(tools),
            Err(first_error) => {
                let _ = first_error;
                self.recover_after_failure().await;
                self.list_tools_once()
                    .await
                    .context("Cua MCP tool discovery failed after reconnect")
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
            biased;
            changed = cancellation.changed() => {
                if changed.is_ok() && *cancellation.borrow() {
                    Self::notify_cancelled(&peer, request_id, "upstream MCP request cancelled").await;
                    return Err(anyhow!(BackendCallCancelled));
                }
                bail!("Cua MCP cancellation channel closed unexpectedly")
            }
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
            _ = sleep(self.tool_timeout) => {
                Self::notify_cancelled(&peer, request_id, "gateway tool timeout").await;
                self.recover_after_failure().await;
                return Err(anyhow!(BackendCallTimedOut {
                    timeout_secs: self.tool_timeout.as_secs(),
                }));
            }
        }
    }

    async fn shutdown(&self) -> Result<()> {
        let _reconnect = self.reconnect_lock.lock().await;
        self.close_current().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    async fn wait_for_file(path: &std::path::Path) {
        timeout(Duration::from_secs(5), async {
            while !path.exists() {
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("fixture marker was not created");
    }

    #[cfg(unix)]
    #[test]
    fn parses_ps_cpu_time_formats() {
        assert_eq!(parse_ps_cpu_time("01:02"), Some(62.0));
        assert_eq!(parse_ps_cpu_time("01:02:03"), Some(3_723.0));
        assert_eq!(parse_ps_cpu_time("2-01:02:03"), Some(176_523.0));
        assert_eq!(parse_ps_cpu_time("00:00.25"), Some(0.25));
        assert_eq!(parse_ps_cpu_time("garbage"), None);
    }

    #[tokio::test]
    async fn propagates_cancellation_to_downstream_request_id() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_nanos();
        let dir = std::env::temp_dir();
        let call_marker = dir.join(format!("cumg-call-{}-{nonce}", std::process::id()));
        let cancel_marker = dir.join(format!("cumg-cancel-{}-{nonce}", std::process::id()));

        let backend = CuaBackend::new(
            "python3",
            vec![
                "scripts/mock_mcp_backend.py".into(),
                "--call-marker".into(),
                call_marker.to_string_lossy().into_owned(),
                "--cancel-marker".into(),
                cancel_marker.to_string_lossy().into_owned(),
            ],
            Duration::from_secs(5),
            Duration::from_secs(30),
            1,
            Duration::from_millis(10),
        );
        backend.connect().await.expect("fixture backend connects");

        let (cancel_tx, cancel_rx) = watch::channel(false);
        let caller = backend.clone();
        let call = tokio::spawn(async move { caller.call_tool("slow", None, cancel_rx).await });

        wait_for_file(&call_marker).await;
        cancel_tx
            .send(true)
            .expect("cancellation receiver is alive");

        let result = timeout(Duration::from_secs(5), call)
            .await
            .expect("cancelled call completes")
            .expect("call task joins");
        assert!(result.is_err(), "cancelled call must not report success");

        wait_for_file(&cancel_marker).await;
        assert_eq!(
            fs::read_to_string(&call_marker).expect("read call marker"),
            fs::read_to_string(&cancel_marker).expect("read cancel marker"),
            "downstream cancellation must reference the in-flight tool request"
        );

        backend
            .shutdown()
            .await
            .expect("fixture backend shuts down");
        let _ = fs::remove_file(call_marker);
        let _ = fs::remove_file(cancel_marker);
    }
}
