// MCP (Model Context Protocol) 客户端模块
// 提供与 MCP 服务器的连接和工具调用功能

#[cfg(not(target_os = "android"))]
pub mod auth;
pub mod client;
pub mod config;
pub mod global;
pub mod http_transport;
pub mod protocol_version;
pub mod rmcp;
pub mod sse_transport;
pub mod stdio_proxy;
pub mod transport;
pub mod types;

// 主要导出
#[cfg(not(target_os = "android"))]
pub use auth::{get_auth_manager, AuthToken, McpAuthManager, OAuth2Token};
pub use client::McpClient;
pub use config::{
    McpConfig, McpFraming, McpPerformanceConfig, McpToolsConfig, McpTransportConfig, OAuthConfig,
};
pub use global::{
    get_global_mcp_client, get_global_mcp_client_sync, initialize_global_mcp_client,
    is_mcp_available, is_mcp_available_sync, set_global_mcp_client, shutdown_global_mcp_client,
};
pub use http_transport::{HttpConfig, HttpTransport};
pub use protocol_version::{CompatibilityChecker, ProtocolNegotiator, ProtocolVersion};
pub use sse_transport::{SSEConfig, SSETransport};
pub use types::*;

/// 判断 MCP endpoint 是否指向本机回环地址。
/// （上游复用 browser::policy::is_loopback_host；fork 无 browser 模块，内联实现：
/// localhost / *.localhost / ::1 / 127.0.0.0/8）
pub(crate) fn endpoint_is_loopback(url: &str) -> bool {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_string))
        .map(|host| {
            let h = host.to_ascii_lowercase();
            h == "localhost"
                || h.ends_with(".localhost")
                || h == "::1"
                || h
                    .parse::<std::net::Ipv4Addr>()
                    .map(|ip| ip.is_loopback())
                    .unwrap_or(false)
        })
        .unwrap_or(false)
}

/// 构建 MCP transport 的 reqwest Client builder。
/// 回环地址必须绕过系统/环境代理：代理无法回连发起方本机端口，
/// 典型症状是本地 SSE/HTTP 连测收到代理返回的 502 Bad Gateway。
pub(crate) fn reqwest_builder_for_endpoint(url: &str) -> reqwest::ClientBuilder {
    let builder = reqwest::Client::builder();
    if endpoint_is_loopback(url) {
        builder.no_proxy()
    } else {
        builder
    }
}
