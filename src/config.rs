use clap::Parser;
use std::net::SocketAddr;

#[derive(Debug, Clone, Parser)]
#[command(name = "computer-use-mcp-gateway")]
#[command(about = "Remote MCP gateway for local computer-use backends")]
pub struct Config {
    /// Address for the public MCP HTTP server. Keep localhost unless a trusted proxy requires otherwise.
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
}
