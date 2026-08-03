//! ser2mcp 库根：模块声明与公共导出。
//!
//! 模块划分：
//! - `hex`    —— 二进制数据与 hex 字符串的编解码（MCP 参数/返回值走文本通道）
//! - `ring`   —— 有界环形缓冲（串口上行数据的囤积区，带溢出计数）
//! - `manager`—— 串口管理器（打开/配置/后台读线程/写）
//! - `server` —— MCP 工具层（工具注册 + ServerHandler 实现）

pub mod hex;
pub mod manager;
pub mod ring;
pub mod server;
