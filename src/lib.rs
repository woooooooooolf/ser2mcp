//! # ser2mcp
//!
//! **UART 串口 MCP 服务器**：把本地串口设备封装成标准的 [MCP (Model Context Protocol)]
//! 工具，让 AI 助手（Claude Desktop、Cursor 及任何 MCP 客户端）直接读写串口。
//!
//! [MCP (Model Context Protocol)]: https://modelcontextprotocol.io
//!
//! ## 架构
//!
//! ```text
//! ┌──────────────┐  JSON-RPC over stdio  ┌──────────────────┐  串口   ┌──────────┐
//! │ MCP 客户端    │ ◄────────────────────► │ ser2mcp          │ ◄─────► │ UART 设备 │
//! │ (AI 助手)     │                        │ 后台读线程+环形缓冲│         │ (TX-RX)  │
//! └──────────────┘                        └──────────────────┘         └──────────┘
//! ```
//!
//! 核心设计：
//! - **上行数据持续囤积、按需拉取**：后台读线程把串口字节流写入有界环形缓冲
//!   （[`ring::RingBuf`]），AI 通过 `uart_read` / `uart_exchange` 按响应单元拉取，
//!   由空闲判定（`idle_ms`）划定数据边界；
//! - **溢出可检测**：缓冲写满后覆盖最旧数据并累计溢出计数，工具返回值中的
//!   `overflow_delta / overflow_total` 让数据缺口对 AI 可见；
//! - **二进制安全**：MCP 通道只保证文本，二进制数据一律以 hex 字符串传递
//!   （[`hex::encode`] / [`hex::decode`]），`mode="text"` 切换 UTF-8 文本。
//!
//! ## 模块
//!
//! - [`hex`]    —— hex 编解码
//! - [`ring`]   —— 有界环形缓冲（覆盖最旧 + 溢出计数 + Notify 唤醒）
//! - [`manager`]—— 串口管理器（打开/重配置/后台读线程/写/拉取）
//! - [`server`] —— MCP 工具层（9 个 `uart_*` 工具 + `ServerHandler`）
//!
//! ## 快速开始
//!
//! ```bash
//! cargo build --release
//! # 注册 target/release/ser2mcp 到任意 MCP 客户端（stdio），详见 README.md
//! ```

#![warn(missing_docs)]

pub mod hex;
pub mod manager;
pub mod ring;
pub mod server;
