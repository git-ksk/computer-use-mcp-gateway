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

    /// Optional comma-separated allowlist. Empty means all discovered tools are eligible.
    #[arg(long, env = "CUMG_ALLOW_TOOLS", default_value = "")]
    pub allow_tools: String,

    /// Optional comma-separated denylist. Deny always wins over allow.
    #[arg(long, env = "CUMG_DENY_TOOLS", default_value = "")]
    pub deny_tools: String,
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
    use super::parse_csv;

    #[test]
    fn parses_csv_without_empty_entries() {
        assert_eq!(
            parse_csv(" screenshot, click ,, type_text "),
            vec!["screenshot", "click", "type_text"]
        );
    }
}
