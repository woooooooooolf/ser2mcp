//! ser2mcp — UART 串口 MCP 服务器入口。
//!
//! 默认以 stdio 传输方式启动 MCP 服务器，供任何 MCP 客户端（AI 助手）调用；
//! 也提供 `--list-ports` / `--version` / `--help` 等命令行辅助能力。

mod hex;
mod manager;
mod reader;
mod ring;
mod sendfile;
mod server;

use anyhow::Result;
use rmcp::{ServiceExt, transport::stdio};
use tracing_subscriber::EnvFilter;

const USAGE: &str = r#"ser2mcp - UART 串口 MCP 服务器

用法:
  ser2mcp               以 stdio 方式启动 MCP 服务器（默认，供 AI 助手调用）
  ser2mcp --list-ports  枚举本机串口后退出
  ser2mcp --version     显示版本号
  ser2mcp --help        显示本帮助

环境变量:
  RUST_LOG  日志级别（默认 info；日志输出到 stderr，不污染 stdio 协议通道）
"#;

fn list_ports() -> i32 {
    let mgr = manager::SerialManager::new();
    match mgr.list_ports() {
        Ok(ports) => {
            println!("本机可用串口（{} 个）:", ports.len());
            for p in &ports {
                println!("  {:<12} {:<10} {}", p.name, p.port_type, p.description);
            }
            0
        }
        Err(e) => {
            eprintln!("枚举串口失败: {e}");
            1
        }
    }
}

async fn run_server() -> Result<()> {
    // MCP 走 stdio，日志必须输出到 stderr，避免污染协议通道。
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
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

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => run_server().await,
        [s] if s == "-h" || s == "--help" => {
            print!("{USAGE}");
            std::process::exit(0);
        }
        [s] if s == "-V" || s == "--version" => {
            println!("ser2mcp {}", env!("CARGO_PKG_VERSION"));
            std::process::exit(0);
        }
        [s] if s == "--list-ports" => std::process::exit(list_ports()),
        [other, ..] => {
            eprintln!("未知参数: {other}\n");
            eprint!("{USAGE}");
            std::process::exit(2);
        }
    }
}
