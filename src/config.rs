use clap::Parser;
use std::net::SocketAddr;

#[derive(Debug, Clone, Parser)]
#[command(name = "computer-use-mcp-gateway")]
#[command(about = "Remote MCP gateway for local computer-use backends")]
pub struct Config {
    /// Address for the MCP HTTP server. Keep localhost unless a trusted proxy requires otherwise.
    #[arg(long, env = "CUMG_BIND", default_value = "127.0.0.1:8100")]
    pub bind: SocketAddr,

    /// MCP endpoint path.
    #[arg(long, env = "CUMG_MCP_PATH", default_value = "/mcp")]
    pub mcp_path: String,

    /// Backend executable.
    #[arg(long, env = "CUMG_BACKEND_COMMAND", default_value = "cua-driver")]
    pub backend_command: String,

    /// Backend arguments, split on ASCII whitespace in V1.
    #[arg(long, env = "CUMG_BACKEND_ARGS", default_value = "mcp")]
    pub backend_args: String,

    /// Comma-separated tool allowlist. Empty is deny-all; `*` explicitly allows every discovered tool.
    #[arg(long, env = "CUMG_ALLOW_TOOLS", default_value = "")]
    pub allow_tools: String,

    /// Optional comma-separated denylist. Deny always wins over allow.
    #[arg(long, env = "CUMG_DENY_TOOLS", default_value = "")]
    pub deny_tools: String,

    /// Allowed inbound Host authorities. Empty keeps the loopback-only secure default.
    #[arg(long, env = "CUMG_ALLOWED_HOSTS", default_value = "")]
    pub allowed_hosts: String,

    /// Allowed browser origins. Empty derives localhost origins from CUMG_BIND.
    #[arg(long, env = "CUMG_ALLOWED_ORIGINS", default_value = "")]
    pub allowed_origins: String,

    /// Maximum concurrent northbound MCP HTTP requests before excess requests fail with HTTP 503.
    #[arg(long, env = "CUMG_MAX_HTTP_CONCURRENCY", default_value_t = 16)]
    pub max_http_concurrency: usize,

    /// Include backend PID/CPU/RSS metadata in /healthz. Disabled by default.
    #[arg(long, env = "CUMG_HEALTH_DETAILS", default_value_t = false)]
    pub health_details: bool,

    /// Timeout for backend MCP connection establishment.
    #[arg(long, env = "CUMG_CONNECT_TIMEOUT_SECS", default_value_t = 15)]
    pub connect_timeout_secs: u64,

    /// Timeout for a single backend MCP operation.
    #[arg(long, env = "CUMG_TOOL_TIMEOUT_SECS", default_value_t = 60)]
    pub tool_timeout_secs: u64,

    /// Number of connection attempts before failing.
    #[arg(long, env = "CUMG_RECONNECT_ATTEMPTS", default_value_t = 3)]
    pub reconnect_attempts: u32,

    /// Initial reconnect backoff in milliseconds; retries use exponential backoff.
    #[arg(long, env = "CUMG_RECONNECT_BACKOFF_MS", default_value_t = 250)]
    pub reconnect_backoff_ms: u64,
}

impl Config {
    pub fn backend_args(&self) -> Vec<String> {
        self.backend_args
            .split_ascii_whitespace()
            .map(ToOwned::to_owned)
            .collect()
    }

    pub fn allow_tools(&self) -> Vec<String> {
        parse_csv(&self.allow_tools)
    }

    pub fn deny_tools(&self) -> Vec<String> {
        parse_csv(&self.deny_tools)
    }

    pub fn allowed_hosts(&self) -> Vec<String> {
        let configured = parse_csv(&self.allowed_hosts);
        if configured.is_empty() {
            vec!["localhost".into(), "127.0.0.1".into(), "::1".into()]
        } else {
            configured
        }
    }

    pub fn allowed_origins(&self) -> Vec<String> {
        let configured = parse_csv(&self.allowed_origins);
        if configured.is_empty() {
            let port = self.bind.port();
            vec![
                format!("http://localhost:{port}"),
                format!("http://127.0.0.1:{port}"),
            ]
        } else {
            configured
        }
    }
}

fn parse_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_csv_without_empty_entries() {
        assert_eq!(
            parse_csv(" screenshot, click ,, type_text "),
            vec!["screenshot", "click", "type_text"]
        );
    }

    #[test]
    fn derives_loopback_transport_guards_by_default() {
        let config = Config::parse_from(["computer-use-mcp-gateway"]);
        assert!(config.allowed_hosts().contains(&"127.0.0.1".to_owned()));
        assert_eq!(
            config.allowed_origins(),
            vec![
                "http://localhost:8100".to_owned(),
                "http://127.0.0.1:8100".to_owned()
            ]
        );
        assert_eq!(config.max_http_concurrency, 16);
        assert!(!config.health_details);
    }

    #[test]
    fn parses_http_hardening_options() {
        let config = Config::parse_from([
            "computer-use-mcp-gateway",
            "--max-http-concurrency",
            "3",
            "--health-details",
        ]);
        assert_eq!(config.max_http_concurrency, 3);
        assert!(config.health_details);
    }
}
