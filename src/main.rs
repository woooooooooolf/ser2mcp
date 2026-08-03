//! ser2mcp — UART 串口 MCP 服务器入口。
//!
//! 以 stdio 传输方式启动 MCP 服务器，供任何 MCP 客户端（AI 助手）调用。

mod hex;
mod manager;
mod ring;
mod server;

use anyhow::Result;
use rmcp::{ServiceExt, transport::stdio};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    // MCP 走 stdio，日志必须输出到 stderr，避免污染协议通道。
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    tracing::info!("ser2mcp 启动：UART 串口 MCP 服务器 (stdio)");

    let service = server::Ser2Mcp::new()
        .serve(stdio())
        .await
        .inspect_err(|e| tracing::error!("服务启动失败: {e:?}"))?;

    service.waiting().await?;
    Ok(())
}
